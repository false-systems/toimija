use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::command::{CommandRunner, command_available};
use crate::{
    config::{CONFIG_PATH, RepoConfig},
    git,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RepoKind {
    Rust,
    Elixir,
    // Minimum-pack languages: data rows in `adapters::pack`, not modules.
    Go,
    Python,
    Node,
    Terraform,
    Generic,
}

impl RepoKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Elixir => "Elixir",
            Self::Go => "Go",
            Self::Python => "Python",
            Self::Node => "JS/TS",
            Self::Terraform => "Terraform",
            Self::Generic => "Generic",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetectedLanguage {
    pub kind: RepoKind,
    #[serde(with = "crate::path_codec::one")]
    pub root: PathBuf,
    #[serde(with = "crate::path_codec::one")]
    pub manifest: PathBuf,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SecondaryScan {
    pub languages: Vec<DetectedLanguage>,
    pub truncated: bool,
    pub incomplete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoInfo {
    pub name: String,
    pub role: Option<String>,
    pub not_roles: Vec<String>,
    pub important_docs: Vec<PathBuf>,
    pub boundaries: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorMode {
    /// Advisory: setup gaps surface as warnings and the command still exits 0.
    Default,
    /// Gate: every actionable warning is escalated to a failure so CI/precommit
    /// can block drift. Used as `toimija doctor --strict`.
    Strict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub serious: bool,
    pub strict: bool,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// The line a failing doctor ends with: the checks that failed, by name,
    /// not a cause they may not share (outside a Git repository nothing
    /// drifted). The report printed above says why each failed.
    pub fn failure_summary(&self) -> String {
        let failed: Vec<&str> = self
            .checks
            .iter()
            .filter(|check| check.status == DoctorStatus::Fail)
            .map(|check| check.name.as_str())
            .collect();
        format!(
            "toimija doctor{}: {} failed ({}); the report above says why and what to do",
            if self.strict { " --strict" } else { "" },
            if failed.len() == 1 {
                "1 check".to_owned()
            } else {
                format!("{} checks", failed.len())
            },
            failed.join(", ")
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorStatus,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorStatus {
    Ok,
    Warning,
    Fail,
}

impl RepoInfo {
    pub fn from_config(config: &RepoConfig, repo_root: &Path) -> Self {
        let name = if config.repo.name.trim().is_empty() {
            repo_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_string()
        } else {
            config.repo.name.clone()
        };
        Self {
            name,
            role: non_empty(&config.repo.role),
            not_roles: config.repo.not_roles.clone(),
            important_docs: config.context.important_docs.clone(),
            boundaries: config.boundaries.clone(),
        }
    }
}

pub fn detect_repo_kind(repo_root: &Path) -> RepoKind {
    crate::adapters::detect(repo_root)
}

const SECONDARY_MAX_DEPTH: usize = 3;
const SECONDARY_ENTRY_BUDGET: usize = 4096;
const SECONDARY_HARD_SKIPS: &[&str] =
    &[".git", "node_modules", "target", "vendor", "_build", "deps"];

/// Find recognized manifests outside the primary language without turning
/// secondary observations into validation claims. Paths come only from
/// `read_dir` child names, symlinks are never followed, and both depth and
/// visited entries are capped.
pub fn detect_secondary_langs(
    repo_root: &Path,
    primary: RepoKind,
    is_excluded: impl Fn(&Path) -> bool,
) -> SecondaryScan {
    detect_secondary_langs_with_budget(repo_root, primary, is_excluded, SECONDARY_ENTRY_BUDGET)
}

fn detect_secondary_langs_with_budget(
    repo_root: &Path,
    primary: RepoKind,
    is_excluded: impl Fn(&Path) -> bool,
    entry_budget: usize,
) -> SecondaryScan {
    let mut scan = SecondaryScan::default();
    let mut queue = VecDeque::from([(PathBuf::new(), 0)]);
    let mut visited = 0;

    'walk: while let Some((relative, depth)) = queue.pop_front() {
        for (kind, manifest) in manifest_claims(repo_root, &relative) {
            push_language(&mut scan, primary, kind, &relative, manifest);
        }
        let entries = match std::fs::read_dir(repo_root.join(&relative)) {
            Ok(entries) => entries,
            Err(_) => {
                scan.incomplete = true;
                continue;
            }
        };
        for entry in entries {
            if visited == entry_budget {
                scan.truncated = true;
                break 'walk;
            }
            visited += 1;
            let Ok(entry) = entry else {
                scan.incomplete = true;
                continue;
            };
            let name = entry.file_name();
            let child = relative.join(&name);
            if SECONDARY_HARD_SKIPS
                .iter()
                .any(|skip| name == std::ffi::OsStr::new(skip))
                || is_excluded(&child)
            {
                continue;
            }
            match std::fs::symlink_metadata(repo_root.join(&child)) {
                Ok(metadata) if metadata.file_type().is_dir() && depth < SECONDARY_MAX_DEPTH => {
                    queue.push_back((child, depth + 1));
                }
                Ok(metadata) if metadata.file_type().is_file() => {
                    let Some(extension) = child.extension().and_then(|ext| ext.to_str()) else {
                        continue;
                    };
                    for language in crate::adapters::pack::LANGUAGES {
                        if language.root_extensions.contains(&extension) {
                            push_language(
                                &mut scan,
                                primary,
                                language.kind,
                                &relative,
                                child.clone(),
                            );
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => scan.incomplete = true,
            }
        }
    }

    scan.languages.sort_by(|left, right| {
        left.root
            .cmp(&right.root)
            .then(kind_rank(left.kind).cmp(&kind_rank(right.kind)))
    });
    scan
}

fn manifest_claims(repo_root: &Path, relative: &Path) -> Vec<(RepoKind, PathBuf)> {
    let mut claims = Vec::new();
    for language in crate::adapters::pack::LANGUAGES {
        if let Some(manifest) = language.manifests.iter().find_map(|manifest| {
            let relative_manifest = relative.join(manifest);
            std::fs::symlink_metadata(repo_root.join(&relative_manifest))
                .ok()
                .filter(|metadata| metadata.file_type().is_file())
                .map(|_| relative_manifest)
        }) {
            claims.push((language.kind, manifest));
        }
    }
    claims
}

fn push_language(
    scan: &mut SecondaryScan,
    primary: RepoKind,
    kind: RepoKind,
    relative: &Path,
    manifest: PathBuf,
) {
    if kind == primary {
        return;
    }
    let root = if relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        relative.to_path_buf()
    };
    if let Some(existing) = scan
        .languages
        .iter_mut()
        .find(|language| language.kind == kind && language.root == root)
    {
        if manifest < existing.manifest {
            existing.manifest = manifest;
        }
    } else {
        scan.languages.push(DetectedLanguage {
            kind,
            root,
            manifest,
        });
    }
}

fn kind_rank(kind: RepoKind) -> usize {
    crate::adapters::pack::LANGUAGES
        .iter()
        .position(|pack| pack.kind == kind)
        .unwrap_or(usize::MAX)
}

pub fn doctor(runner: &impl CommandRunner, cwd: &Path, mode: DoctorMode) -> DoctorReport {
    let strict = mode == DoctorMode::Strict;
    let mut checks = Vec::new();
    let git_available = command_available("git");
    checks.push(check(
        "git installed",
        if git_available {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Fail
        },
        if git_available {
            "git found"
        } else {
            "git not found"
        },
    ));
    let repo_root = match git::discover_repo_root(runner, cwd) {
        Ok(root) => {
            checks.push(check(
                "inside git repo",
                DoctorStatus::Ok,
                "git repo detected",
            ));
            root
        }
        Err(_) => {
            checks.push(check(
                "inside git repo",
                DoctorStatus::Fail,
                "not a Git repository",
            ));
            return DoctorReport {
                serious: true,
                strict,
                checks,
            };
        }
    };
    let config = match RepoConfig::load_optional(&repo_root) {
        Ok(Some(config)) => {
            checks.push(check("toimija config", DoctorStatus::Ok, "config found"));
            config
        }
        Ok(None) => {
            checks.push(check(
                "toimija config",
                DoctorStatus::Warning,
                "missing config; run `toimija init`",
            ));
            RepoConfig::default_for(&repo_root)
        }
        Err(error) => {
            checks.push(check(
                "toimija config",
                DoctorStatus::Fail,
                &format!("{error}"),
            ));
            return DoctorReport {
                serious: true,
                strict,
                checks,
            };
        }
    };
    checks.push(check(
        "repo name",
        if config.repo.name.trim().is_empty() {
            DoctorStatus::Warning
        } else {
            DoctorStatus::Ok
        },
        if config.repo.name.trim().is_empty() {
            "repo name is empty"
        } else {
            "repo name present"
        },
    ));
    checks.push(check(
        "repo role",
        if config.repo.role.trim().is_empty() {
            DoctorStatus::Warning
        } else {
            DoctorStatus::Ok
        },
        if config.repo.role.trim().is_empty() {
            "repo role is empty"
        } else {
            "repo role present"
        },
    ));
    checks.push(check(
        "boundaries",
        if config.boundaries.is_empty() {
            DoctorStatus::Warning
        } else {
            DoctorStatus::Ok
        },
        if config.boundaries.is_empty() {
            "boundaries are empty"
        } else {
            "boundaries present"
        },
    ));
    let kind = detect_repo_kind(&repo_root);
    checks.push(check(
        "repo kind",
        DoctorStatus::Ok,
        crate::adapters::pack::for_kind(kind)
            .map(|pack| pack.doctor_message)
            .unwrap_or("generic repo"),
    ));
    checks.push(check(
        "validation plan",
        if config.has_validation() || kind != RepoKind::Generic {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Warning
        },
        if config.has_validation() {
            "validation configured"
        } else if kind != RepoKind::Generic {
            "validation can be inferred"
        } else {
            "validation unknown"
        },
    ));
    checks.push(packet_ignored_check(runner, &repo_root));
    checks.push(file_check(&repo_root, "AGENTS.md", "AGENTS.md"));
    let claude = repo_root.join("CLAUDE.md");
    if claude.exists() {
        let text = std::fs::read_to_string(&claude).unwrap_or_default();
        checks.push(check(
            "CLAUDE.md",
            if text.contains("AGENTS.md") {
                DoctorStatus::Ok
            } else {
                DoctorStatus::Warning
            },
            if text.contains("AGENTS.md") {
                "CLAUDE.md references AGENTS.md"
            } else {
                "CLAUDE.md exists but does not reference AGENTS.md"
            },
        ));
    } else {
        checks.push(check("CLAUDE.md", DoctorStatus::Ok, "CLAUDE.md absent"));
    }

    // Drift guard: the agent docs are the contract a coding agent reads. If they
    // stop referring people to the preflight, or stop reflecting the configured
    // boundaries, the contract has silently rotted even though every file still
    // parses. These checks are advisory by default and gate under `--strict`.
    let docs_text = read_canonical_docs(&repo_root, &config);
    let has_preflight = PREFLIGHT_MARKERS
        .iter()
        .any(|marker| docs_text.contains(marker));
    checks.push(check(
        "preflight mention",
        if has_preflight {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Warning
        },
        if has_preflight {
            "agent docs point at the `toimija enter` preflight"
        } else {
            "no agent doc mentions the `toimija enter` preflight"
        },
    ));

    let missing_docs: Vec<&PathBuf> = config
        .context
        .important_docs
        .iter()
        .filter(|doc| !repo_root.join(doc).exists())
        .collect();
    checks.push(check(
        "important docs",
        if missing_docs.is_empty() {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Warning
        },
        &if missing_docs.is_empty() {
            "important_docs paths exist".to_string()
        } else {
            format!("missing on disk: {}", join_paths(&missing_docs))
        },
    ));

    if !config.boundaries.is_empty() {
        // Boundaries can be satisfied two drift-free ways: mirror them verbatim
        // into the agent docs, OR have the docs point at `.toimija/repo.yaml` as
        // the single source. A reference keeps one source of truth — the docs and
        // the manifest cannot disagree — so it counts as satisfied without
        // verbatim duplication.
        let sourced_by_reference = docs_text.contains(CONFIG_PATH);
        let unmirrored: Vec<&String> = config
            .boundaries
            .iter()
            .filter(|boundary| !boundary_present(boundary, &docs_text))
            .collect();
        let satisfied = sourced_by_reference || unmirrored.is_empty();
        checks.push(check(
            "boundaries mirrored",
            if satisfied {
                DoctorStatus::Ok
            } else {
                DoctorStatus::Warning
            },
            &if unmirrored.is_empty() {
                "boundaries appear in the agent docs".to_string()
            } else if sourced_by_reference {
                format!("boundaries sourced by reference to {CONFIG_PATH}")
            } else {
                format!(
                    "{} boundary line(s) missing from agent docs: {}",
                    unmirrored.len(),
                    unmirrored
                        .iter()
                        .map(|b| format!("{b:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
        ));
    }

    // Strict turns every actionable warning into a failure so a CI/precommit
    // gate exits non-zero. Checks that are already Ok (e.g. an absent CLAUDE.md)
    // stay Ok — strict raises the floor, it does not invent new requirements.
    if strict {
        for check in &mut checks {
            if check.status == DoctorStatus::Warning {
                check.status = DoctorStatus::Fail;
            }
        }
    }

    let serious = checks
        .iter()
        .any(|check| check.status == DoctorStatus::Fail);
    DoctorReport {
        serious,
        strict,
        checks,
    }
}

/// Substrings the agent docs may contain to prove they still route work through
/// a Toimija preflight. `enter` is the recommended entry command; `context` is
/// the underlying one it shares — either satisfies the guard. Deliberately the
/// command stem, not a full sentence, so wording around it can vary.
const PREFLIGHT_MARKERS: [&str; 3] = ["toimija enter", "toimija context", ".toimija/current.md"];

/// Concatenated bodies of the docs that form the agent contract: AGENTS.md,
/// CLAUDE.md, and whatever `important_docs` nominates as load-bearing. Unreadable
/// or absent files contribute nothing rather than erroring — a missing AGENTS.md
/// is already caught by its own check.
fn read_canonical_docs(repo_root: &Path, config: &RepoConfig) -> String {
    let mut names = vec![PathBuf::from("AGENTS.md"), PathBuf::from("CLAUDE.md")];
    names.extend(config.context.important_docs.iter().cloned());
    names
        .iter()
        .filter_map(|name| std::fs::read_to_string(repo_root.join(name)).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drift test for one boundary: does its normalized form still appear somewhere
/// in the normalized doc text? Normalization (lowercase, whitespace collapsed,
/// trailing sentence punctuation trimmed) absorbs casing, re-wrapping, and a
/// stray full stop while still failing when the sentence is genuinely gone. This
/// is intentionally lexical, not semantic — a boring drift guard, not a model.
fn boundary_present(boundary: &str, docs_text: &str) -> bool {
    let needle = normalize(boundary);
    !needle.is_empty() && normalize(docs_text).contains(&needle)
}

fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .trim_end_matches(['.', ',', ';', ':', '!', '?'])
        .to_string()
}

fn join_paths(paths: &[&PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Everything `init` ignores, as `(path Git is asked about, label)`: one probe
/// per rule in [`crate::config::IGNORE_RULES`], so the check and the rules
/// cannot drift. A directory rule is probed with a file inside it, which Git
/// answers for by the directory's rule. `tmp/` is left out: it ignores itself.
fn packet_paths() -> Vec<(String, String)> {
    crate::config::IGNORE_RULES
        .lines()
        .filter_map(|line| line.strip_prefix('/'))
        .filter(|rule| *rule != "tmp/")
        .map(|rule| {
            let label = format!(".toimija/{rule}");
            let probe = if rule.ends_with('/') {
                format!("{label}0")
            } else {
                label.clone()
            };
            (probe, label)
        })
        .collect()
}

/// Whether Git ignores what Toimija generates, by whatever rules it reads (the
/// repo's `.gitignore` files, `info/exclude`, the global excludes). A packet
/// Git can see makes the first refresh report its own files as dirty work —
/// and a tracked one changes on every refresh.
fn packet_ignored_check(runner: &impl CommandRunner, repo_root: &Path) -> DoctorCheck {
    let mut unignored = Vec::new();
    for (probe, label) in packet_paths() {
        match runner.run(repo_root, "git", &["check-ignore", "-q", &probe]) {
            Ok(output) if output.status == 0 => {}
            Ok(output) if output.status == 1 => unignored.push(label),
            _ => {
                return check(
                    "packet ignored",
                    DoctorStatus::Warning,
                    "could not ask Git whether it ignores the packet (`git check-ignore` failed)",
                );
            }
        }
    }
    if unignored.is_empty() {
        check(
            "packet ignored",
            DoctorStatus::Ok,
            "Git ignores the packet, sessions, history, runs and snapshots",
        )
    } else {
        check(
            "packet ignored",
            DoctorStatus::Warning,
            &format!(
                "Git does not ignore {}; a refresh shows them as untracked work — \
                 `toimija init` writes {} (or add them to your ignore rules)",
                unignored.join(", "),
                crate::config::IGNORE_PATH
            ),
        )
    }
}

fn file_check(repo_root: &Path, name: &str, path: &str) -> DoctorCheck {
    let present = repo_root.join(path).exists();
    check(
        name,
        if present {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Warning
        },
        if present {
            "file present"
        } else {
            "file missing"
        },
    )
}

fn check(name: &str, status: DoctorStatus, message: &str) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        status,
        message: message.to_string(),
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust_repo() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/rust_repo");
        assert_eq!(detect_repo_kind(&root), RepoKind::Rust);
    }

    #[test]
    fn detects_elixir_repo() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/elixir_repo");
        assert_eq!(detect_repo_kind(&root), RepoKind::Elixir);
    }

    #[test]
    fn secondary_walk_is_bounded_and_suppresses_primary_kind() {
        let temp = tempfile::tempdir().unwrap();
        for path in [
            "services/api",
            "infra",
            "xtask",
            "node_modules/pkg",
            "a/b/c/d",
        ] {
            std::fs::create_dir_all(temp.path().join(path)).unwrap();
        }
        std::fs::write(temp.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(temp.path().join("package.json"), "{}\n").unwrap();
        std::fs::write(temp.path().join("services/api/go.mod"), "module demo\n").unwrap();
        std::fs::write(temp.path().join("infra/network.tf"), "").unwrap();
        std::fs::write(temp.path().join("xtask/Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(temp.path().join("node_modules/pkg/package.json"), "{}\n").unwrap();
        std::fs::write(temp.path().join("a/b/c/d/mix.exs"), "[]\n").unwrap();

        let scan = detect_secondary_langs(temp.path(), RepoKind::Rust, |_| false);
        assert_eq!(
            scan.languages,
            vec![
                DetectedLanguage {
                    kind: RepoKind::Node,
                    root: PathBuf::from("."),
                    manifest: PathBuf::from("package.json"),
                },
                DetectedLanguage {
                    kind: RepoKind::Terraform,
                    root: PathBuf::from("infra"),
                    manifest: PathBuf::from("infra/network.tf"),
                },
                DetectedLanguage {
                    kind: RepoKind::Go,
                    root: PathBuf::from("services/api"),
                    manifest: PathBuf::from("services/api/go.mod"),
                },
            ]
        );
        assert!(!scan.truncated);
    }

    #[test]
    fn secondary_walk_honors_configured_exclusions() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("fixtures/elixir_repo")).unwrap();
        std::fs::write(temp.path().join("fixtures/elixir_repo/mix.exs"), "[]\n").unwrap();

        let scan = detect_secondary_langs(temp.path(), RepoKind::Generic, |path| {
            path.starts_with("fixtures")
        });
        assert!(scan.languages.is_empty());
    }

    #[test]
    fn secondary_walk_reports_entry_budget_exhaustion() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("one"), "").unwrap();
        std::fs::write(temp.path().join("two"), "").unwrap();

        let scan = detect_secondary_langs_with_budget(temp.path(), RepoKind::Generic, |_| false, 1);
        assert!(scan.truncated);
    }

    #[test]
    fn secondary_extension_detection_obeys_entry_budget() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("network.tf"), "").unwrap();

        let scan = detect_secondary_langs_with_budget(temp.path(), RepoKind::Generic, |_| false, 0);
        assert!(scan.truncated);
        assert!(scan.languages.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn secondary_walk_never_follows_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("go.mod"), "module outside\n").unwrap();
        symlink(outside.path(), temp.path().join("linked")).unwrap();

        let scan = detect_secondary_langs(temp.path(), RepoKind::Generic, |_| false);
        assert!(scan.languages.is_empty());
    }

    #[test]
    fn a_failing_doctor_names_only_the_checks_that_failed() {
        let report = |strict| DoctorReport {
            serious: true,
            strict,
            checks: vec![
                check("git installed", DoctorStatus::Ok, "git found"),
                check("preflight mention", DoctorStatus::Warning, "none"),
                check("toimija config", DoctorStatus::Fail, "invalid"),
                check("packet ignored", DoctorStatus::Fail, "not ignored"),
            ],
        };
        assert_eq!(
            report(false).failure_summary(),
            "toimija doctor: 2 checks failed (toimija config, packet ignored); \
             the report above says why and what to do"
        );
        assert_eq!(
            report(true).failure_summary(),
            "toimija doctor --strict: 2 checks failed (toimija config, packet ignored); \
             the report above says why and what to do"
        );
    }

    #[test]
    fn boundary_matches_through_casing_and_trailing_period() {
        let boundary = "Keep v0.0 as one Rust crate.";
        let docs = "# Agent guide\n\nkeep v0.0 as one rust crate at all times.\n";
        assert!(boundary_present(boundary, docs));
    }

    #[test]
    fn boundary_matches_through_line_wrapping() {
        // A boundary re-wrapped across lines in the doc still normalizes to the
        // same single-spaced form, so the drift guard does not fire on layout.
        let boundary = "Do not add network calls or background tasks.";
        let docs = "Do not add network\ncalls or background\ntasks for now.";
        assert!(boundary_present(boundary, docs));
    }

    #[test]
    fn missing_boundary_is_detected() {
        let boundary = "Keep v0.0 as one Rust crate.";
        let docs = "# Agent guide\n\nRun the tests before committing.\n";
        assert!(!boundary_present(boundary, docs));
    }

    #[test]
    fn blank_boundary_never_counts_as_present() {
        // A whitespace-only boundary normalizes to "" — guard against it
        // trivially matching every document.
        assert!(!boundary_present("   ", "any document text"));
    }
}
