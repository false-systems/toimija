//! The compile-time language registry. Every recognized language starts as one
//! data row; smart adapters override only the behavior that needs project-aware
//! code, while bounded proof rules keep inferred commands honest. No source
//! parsing — manifests and path templates only — and `repo.yaml` validation
//! config still beats everything inferred from a row.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::adapters::LanguageAdapter;
use crate::repo::RepoKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageTier {
    Full,
    Minimum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationProof {
    Manifest,
    NodeScripts,
    PythonTools,
}

#[derive(Debug)]
pub struct LanguagePack {
    pub kind: RepoKind,
    pub tier: LanguageTier,
    pub proof: ValidationProof,
    /// Honest display label for notes and telemetry (e.g. "Go").
    pub name: &'static str,
    /// Root-manifest evidence rendered in context packets.
    pub root_evidence: &'static str,
    /// Repository-kind result rendered by `toimija doctor`.
    pub doctor_message: &'static str,
    /// Any of these at the repo root claims the repo, checked in table order.
    pub manifests: &'static [&'static str],
    /// A root file with any of these extensions claims repos without a
    /// canonical manifest, checked after exact manifest names.
    pub root_extensions: &'static [&'static str],
    /// Fast-tier (test) candidates. Proven commands also become the Full tier,
    /// mirroring the full adapters' shape.
    pub fast: &'static [&'static str],
    /// Strict-tier (lint/vet) candidates.
    pub strict: &'static [&'static str],
    /// Source extensions the companion templates apply to.
    pub extensions: &'static [&'static str],
    /// Companion-test path templates: `{dir}`, `{stem}`, `{ext}` expand from
    /// the changed file. Candidates are existence-filtered, same as the full
    /// adapters — a hint never points at a file that is not there.
    pub companions: &'static [&'static str],
    /// A changed file whose name matches one of these is already a test;
    /// tests do not get companion hints for themselves. Anchored glob-lite:
    /// `test_*` = prefix, `*_test.go` = suffix, `*.test.*` = contains —
    /// anchoring matters, or `latest_config.py` reads as a test.
    pub test_markers: &'static [&'static str],
}

pub const LANGUAGES: &[LanguagePack] = &[
    LanguagePack {
        kind: RepoKind::Rust,
        tier: LanguageTier::Full,
        proof: ValidationProof::Manifest,
        name: "Rust",
        root_evidence: "Cargo.toml at root",
        doctor_message: "Rust repo detected",
        manifests: &["Cargo.toml"],
        root_extensions: &[],
        fast: &["cargo test --workspace"],
        strict: &[
            "cargo fmt --check",
            "cargo clippy --workspace --all-targets -- -D warnings",
        ],
        extensions: &["rs"],
        companions: &[],
        test_markers: &[],
    },
    LanguagePack {
        kind: RepoKind::Elixir,
        tier: LanguageTier::Full,
        proof: ValidationProof::Manifest,
        name: "Elixir",
        root_evidence: "mix.exs at root",
        doctor_message: "Elixir repo detected",
        manifests: &["mix.exs"],
        root_extensions: &[],
        fast: &["mix test"],
        strict: &["mix format --check-formatted"],
        extensions: &["ex", "exs"],
        companions: &[],
        test_markers: &[],
    },
    LanguagePack {
        kind: RepoKind::Go,
        tier: LanguageTier::Minimum,
        proof: ValidationProof::Manifest,
        name: "Go",
        root_evidence: "go.mod at root",
        doctor_message: "Go repo detected (minimum language pack)",
        manifests: &["go.mod"],
        root_extensions: &[],
        fast: &["go test ./..."],
        strict: &["go vet ./..."],
        extensions: &["go"],
        companions: &["{dir}/{stem}_test.go"],
        test_markers: &["*_test.go"],
    },
    LanguagePack {
        kind: RepoKind::Python,
        tier: LanguageTier::Minimum,
        proof: ValidationProof::PythonTools,
        name: "Python",
        root_evidence: "pyproject.toml or setup.py at root",
        doctor_message: "Python repo detected (minimum language pack)",
        manifests: &["pyproject.toml", "setup.py"],
        root_extensions: &[],
        fast: &["pytest"],
        strict: &["ruff check ."],
        extensions: &["py"],
        companions: &["{dir}/test_{stem}.py", "tests/test_{stem}.py"],
        test_markers: &["test_*"],
    },
    LanguagePack {
        kind: RepoKind::Node,
        tier: LanguageTier::Minimum,
        proof: ValidationProof::NodeScripts,
        name: "JS/TS",
        root_evidence: "package.json at root",
        doctor_message: "JS/TS repo detected (minimum language pack)",
        manifests: &["package.json"],
        root_extensions: &[],
        fast: &["npm test"],
        strict: &["npm run lint", "npm run typecheck"],
        extensions: &["ts", "tsx", "js", "jsx"],
        companions: &["{dir}/{stem}.test.{ext}", "{dir}/{stem}.spec.{ext}"],
        test_markers: &[".test.", ".spec."],
    },
    LanguagePack {
        kind: RepoKind::Terraform,
        tier: LanguageTier::Minimum,
        proof: ValidationProof::Manifest,
        name: "Terraform",
        root_evidence: ".tf file at root",
        doctor_message: "Terraform repo detected (minimum language pack)",
        manifests: &[],
        root_extensions: &["tf"],
        fast: &[],
        strict: &["terraform fmt -check -recursive"],
        extensions: &["tf"],
        companions: &[],
        test_markers: &[],
    },
];

/// The first recognized language with a root marker.
/// Registry order is precedence: full adapters first, then minimum packs.
pub fn detect(repo_root: &Path) -> Option<&'static LanguagePack> {
    LANGUAGES
        .iter()
        .find(|lang| lang.root_marker(repo_root).is_some())
}

pub fn for_kind(kind: RepoKind) -> Option<&'static LanguagePack> {
    LANGUAGES.iter().find(|lang| lang.kind == kind)
}

#[derive(Debug, Default)]
pub(crate) struct ProvenCommands {
    pub fast: Vec<String>,
    pub strict: Vec<String>,
    pub notes: Vec<String>,
}

pub(crate) fn proven_commands(repo_root: &Path, lang: &LanguagePack) -> ProvenCommands {
    match lang.proof {
        ValidationProof::Manifest => ProvenCommands {
            fast: strings(lang.fast),
            strict: strings(lang.strict),
            notes: Vec::new(),
        },
        ValidationProof::NodeScripts => node_commands(repo_root, lang),
        ValidationProof::PythonTools => python_commands(repo_root, lang),
    }
}

fn node_commands(repo_root: &Path, lang: &LanguagePack) -> ProvenCommands {
    let mut commands = ProvenCommands::default();
    let scripts = match read_bounded(&repo_root.join("package.json")) {
        Ok(contents) => match serde_json::from_str::<serde_json::Value>(&contents) {
            Ok(manifest) => manifest
                .get("scripts")
                .and_then(serde_json::Value::as_object)
                .cloned(),
            Err(_) => return all_dropped(lang, "package.json is malformed"),
        },
        Err(reason) => return all_dropped(lang, reason),
    };

    for command in lang.fast {
        if has_string_script(scripts.as_ref(), "test") {
            commands.fast.push((*command).to_string());
        } else {
            commands.notes.push(format!(
                "{command} not inferred — no test script in package.json"
            ));
        }
    }
    for command in lang.strict {
        let script = command.strip_prefix("npm run ").unwrap_or(command);
        if has_string_script(scripts.as_ref(), script) {
            commands.strict.push((*command).to_string());
        } else {
            commands.notes.push(format!(
                "{command} not inferred — no {script} script in package.json"
            ));
        }
    }
    commands
}

fn python_commands(repo_root: &Path, lang: &LanguagePack) -> ProvenCommands {
    let pyproject = read_bounded(&repo_root.join("pyproject.toml")).unwrap_or_default();
    let mut commands = ProvenCommands::default();

    if has_table(&pyproject, "tool.pytest.ini_options") || repo_root.join("pytest.ini").is_file() {
        commands.fast = strings(lang.fast);
    } else {
        commands
            .notes
            .push("pytest not inferred — no [tool.pytest.ini_options] or pytest.ini".to_string());
    }

    if has_table(&pyproject, "tool.ruff")
        || repo_root.join("ruff.toml").is_file()
        || repo_root.join(".ruff.toml").is_file()
    {
        commands.strict = strings(lang.strict);
    } else {
        commands.notes.push(
            "ruff check . not inferred — no [tool.ruff], ruff.toml, or .ruff.toml".to_string(),
        );
    }
    commands
}

fn has_string_script(
    scripts: Option<&serde_json::Map<String, serde_json::Value>>,
    name: &str,
) -> bool {
    scripts
        .and_then(|scripts| scripts.get(name))
        .is_some_and(serde_json::Value::is_string)
}

fn has_table(contents: &str, expected: &str) -> bool {
    contents.lines().any(|line| {
        line.trim()
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
            .map(str::trim)
            == Some(expected)
    })
}

fn strings(commands: &[&str]) -> Vec<String> {
    commands
        .iter()
        .map(|command| (*command).to_string())
        .collect()
}

fn all_dropped(lang: &LanguagePack, reason: &str) -> ProvenCommands {
    ProvenCommands {
        notes: lang
            .fast
            .iter()
            .chain(lang.strict)
            .map(|command| format!("{command} not inferred — {reason}"))
            .collect(),
        ..ProvenCommands::default()
    }
}

const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

fn read_bounded(path: &Path) -> Result<String, &'static str> {
    let file = File::open(path).map_err(|_| "manifest could not be read")?;
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "manifest could not be read")?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("manifest exceeds 256 KiB");
    }
    String::from_utf8(bytes).map_err(|_| "manifest is not UTF-8")
}

/// The baseline adapter for a declarative pack. A language graduates to a
/// dedicated adapter only when it gains behavior that cannot live in the row.
#[derive(Debug, Clone, Copy)]
pub struct PackAdapter(pub &'static LanguagePack);

impl LanguageAdapter for PackAdapter {
    fn kind(&self) -> RepoKind {
        self.0.kind
    }
}

impl LanguagePack {
    pub fn root_marker(&self, root: &Path) -> Option<PathBuf> {
        if let Some(manifest) = self.manifests.iter().find(|manifest| {
            std::fs::symlink_metadata(root.join(manifest))
                .is_ok_and(|metadata| metadata.file_type().is_file())
        }) {
            return Some(PathBuf::from(manifest));
        }
        if self.root_extensions.is_empty() {
            return None;
        }
        let mut matches = std::fs::read_dir(root)
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                if !entry.file_type().ok()?.is_file() {
                    return None;
                }
                let path = entry.path();
                let extension = path.extension()?.to_str()?;
                self.root_extensions
                    .contains(&extension)
                    .then_some(entry.file_name())
            })
            .collect::<Vec<_>>();
        matches.sort();
        matches.into_iter().next().map(PathBuf::from)
    }

    pub fn companion_tests(&self, repo_root: &Path, path: &Path) -> Vec<PathBuf> {
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            return Vec::new();
        };
        if !self.extensions.contains(&ext) {
            return Vec::new();
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Vec::new();
        };
        if self
            .test_markers
            .iter()
            .any(|marker| matches_marker(name, marker))
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
        self.companions
            .iter()
            .map(|template| expand_template(template, &dir, stem, ext))
            .filter(|candidate| repo_root.join(candidate).exists())
            .collect()
    }
}

/// Anchored glob-lite for test markers: `test_*` matches a prefix,
/// `*_test.go` a suffix, `*.test.*` a substring, and a bare word exactly.
/// Anchoring is the point — a plain `contains` would mark `latest_config.py`
/// as a test and silently drop its companion hint.
fn matches_marker(name: &str, marker: &str) -> bool {
    match (marker.strip_prefix('*'), marker.strip_suffix('*')) {
        (Some(_), Some(_)) => name.contains(marker.trim_matches('*')),
        (Some(suffix), None) => name.ends_with(suffix),
        (None, Some(prefix)) => name.starts_with(prefix),
        (None, None) => name == marker,
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

    fn adapter(kind: RepoKind) -> PackAdapter {
        PackAdapter(for_kind(kind).unwrap())
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
        // Full adapters outrank minimum packs through the same registry.
        std::fs::write(temp.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        assert_eq!(detect(temp.path()).unwrap().kind, RepoKind::Rust);
    }

    #[test]
    fn terraform_detection_accepts_any_root_tf_file() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("fake.tf")).unwrap();
        std::fs::write(temp.path().join("network.tf"), "").unwrap();

        let terraform = detect(temp.path()).unwrap();
        assert_eq!(terraform.kind, RepoKind::Terraform);
        assert_eq!(
            terraform.root_marker(temp.path()),
            Some(PathBuf::from("network.tf"))
        );
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
    fn markers_are_anchored_not_substring() {
        // `latest_config.py` contains "test_" but is not a test — it must
        // still get its companion hint.
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("tests")).unwrap();
        std::fs::write(temp.path().join("tests/test_latest_config.py"), "").unwrap();
        let python = adapter(RepoKind::Python);
        assert_eq!(
            python.companion_tests(temp.path(), Path::new("latest_config.py")),
            vec![PathBuf::from("tests/test_latest_config.py")]
        );
        // `my_tester.go` contains "_test" but is not `*_test.go` — hinted too.
        std::fs::write(temp.path().join("my_tester_test.go"), "").unwrap();
        let go = adapter(RepoKind::Go);
        assert_eq!(
            go.companion_tests(temp.path(), Path::new("my_tester.go")),
            vec![PathBuf::from("my_tester_test.go")]
        );

        assert!(matches_marker("test_foo.py", "test_*"));
        assert!(!matches_marker("latest_config.py", "test_*"));
        assert!(matches_marker("foo_test.go", "*_test.go"));
        assert!(!matches_marker("my_tester.go", "*_test.go"));
        assert!(matches_marker("app.test.ts", "*.test.*"));
        assert!(!matches_marker("apptest.ts", "*.test.*"));
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

    #[test]
    fn node_commands_require_string_scripts() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"test":"vitest","lint":"eslint .","typecheck":"tsc"}}"#,
        )
        .unwrap();

        let commands = proven_commands(temp.path(), for_kind(RepoKind::Node).unwrap());
        assert_eq!(commands.fast, ["npm test"]);
        assert_eq!(commands.strict, ["npm run lint", "npm run typecheck"]);
        assert!(commands.notes.is_empty());
    }

    #[test]
    fn malformed_node_manifest_drops_each_candidate_once() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("package.json"), "{}").unwrap();
        let missing = proven_commands(temp.path(), for_kind(RepoKind::Node).unwrap());
        assert_eq!(missing.notes.len(), 3);
        assert!(missing.notes.iter().all(|note| note.contains("no ")));

        std::fs::write(temp.path().join("package.json"), "{").unwrap();

        let commands = proven_commands(temp.path(), for_kind(RepoKind::Node).unwrap());
        assert!(commands.fast.is_empty());
        assert!(commands.strict.is_empty());
        assert_eq!(commands.notes.len(), 3);
        assert!(commands.notes.iter().all(|note| note.contains("malformed")));
    }

    #[test]
    fn manifest_reads_are_bounded() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("package.json"),
            vec![b' '; MAX_MANIFEST_BYTES as usize + 1],
        )
        .unwrap();

        let commands = proven_commands(temp.path(), for_kind(RepoKind::Node).unwrap());
        assert_eq!(commands.notes.len(), 3);
        assert!(commands.notes.iter().all(|note| note.contains("256 KiB")));
    }

    #[test]
    fn python_commands_require_explicit_evidence() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("setup.py"),
            "from setuptools import setup\n",
        )
        .unwrap();
        std::fs::create_dir(temp.path().join("tests")).unwrap();
        let python = for_kind(RepoKind::Python).unwrap();

        let dropped = proven_commands(temp.path(), python);
        assert!(dropped.fast.is_empty());
        assert!(dropped.strict.is_empty());
        assert_eq!(dropped.notes.len(), 2);

        std::fs::write(temp.path().join("pytest.ini"), "[pytest]\n").unwrap();
        std::fs::write(temp.path().join("ruff.toml"), "line-length = 100\n").unwrap();
        let proven = proven_commands(temp.path(), python);
        assert_eq!(proven.fast, ["pytest"]);
        assert_eq!(proven.strict, ["ruff check ."]);
        assert!(proven.notes.is_empty());
    }

    #[test]
    fn python_tables_are_section_aware() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("pyproject.toml"),
            "[tool.pytest.ini_options]\naddopts = \"-q\"\n[tool.ruff]\nline-length = 100\n",
        )
        .unwrap();

        let commands = proven_commands(temp.path(), for_kind(RepoKind::Python).unwrap());
        assert_eq!(commands.fast, ["pytest"]);
        assert_eq!(commands.strict, ["ruff check ."]);
    }
}
