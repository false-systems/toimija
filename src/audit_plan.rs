//! Deterministic, Git-derived worksets for breadth-scoped audits.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::command::CommandRunner;
use crate::config::RepoConfig;
use crate::consistency::Consistency;
use crate::context::{effective_ignore_rules, matches_safe_to_ignore};
use crate::git::{self, GitState, run_git_ok};
use crate::relevance::{DEFAULT_LOG_WINDOW_COMMITS, mine_log_commits};
use crate::repo;
use crate::validate::{self, ValidationPlan};

const MAX_SURFACES: usize = 8;
const TARGET_FILES_PER_SURFACE: usize = 150;
const ROOT_AREA: &str = "(root files)";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditArea {
    pub label: String,
    pub files: usize,
    pub churn: usize,
}

impl AuditArea {
    fn weight(&self) -> usize {
        // Additive weight keeps file breadth and historical activity visible
        // without pretending either signal is semantic understanding.
        self.files + self.churn
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditSurface {
    pub areas: Vec<AuditArea>,
    pub files: usize,
    pub churn: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditPlan {
    pub repo_root: PathBuf,
    pub head: Option<String>,
    pub consistency: String,
    pub surfaces: Vec<AuditSurface>,
    pub prior_findings: Vec<PathBuf>,
    pub findings_root: Option<PathBuf>,
    pub validation_plan: ValidationPlan,
    pub warnings: Vec<String>,
    pub default_budget: usize,
}

pub fn build(
    runner: &impl CommandRunner,
    cwd: &Path,
    requested_surfaces: Option<usize>,
) -> Result<AuditPlan> {
    let (git, mut warnings) =
        git::collect_git_state(runner, cwd).context("failed to read Git state")?;
    let config = match RepoConfig::load_optional(&git.repo_root)? {
        Some(config) => config,
        None => {
            warnings.push("Missing repo.yaml; run `toimija init`.".to_string());
            RepoConfig::default_for(&git.repo_root)
        }
    };
    let repo_kind = repo::detect_repo_kind(&git.repo_root);
    if repo_kind == repo::RepoKind::Generic {
        warnings.push("Repo kind is generic; validation may be unavailable.".to_string());
    }
    let validation_plan = validate::validation_plan(&git.repo_root, &config, repo_kind);
    let ignore_rules = effective_ignore_rules(&config.context.safe_to_ignore);
    let tracked = tracked_files(runner, &git)?;
    let areas = area_stats(&tracked, &ignore_rules);
    let history = mine_log_commits(runner, &git.repo_root, "HEAD", DEFAULT_LOG_WINDOW_COMMITS);
    if history.as_ref().is_none_or(Vec::is_empty) {
        warnings.push(history_warning());
    }
    let areas = with_churn(areas, history.as_deref());
    let surface_count = requested_surfaces
        .map(|n| n.clamp(1, areas.len().max(1)))
        .unwrap_or_else(|| default_surface_count(&areas));
    let surfaces = pack_areas(areas, surface_count);
    let (findings_root, prior_findings) = prior_findings(runner, &git)?;
    let consistency = Consistency::fingerprint(&git);
    warnings.sort();
    warnings.dedup();
    Ok(AuditPlan {
        repo_root: git.repo_root,
        head: git.head_oid,
        consistency: consistency.token,
        surfaces,
        prior_findings,
        findings_root,
        validation_plan,
        warnings,
        default_budget: config.context.default_budget,
    })
}

fn history_warning() -> String {
    format!(
        "Audit churn unavailable: history mining failed; weighting uses file counts only (last {DEFAULT_LOG_WINDOW_COMMITS} non-merge commits)."
    )
}

fn tracked_files(runner: &impl CommandRunner, git: &GitState) -> Result<Vec<PathBuf>> {
    let Some(output) = run_git_ok(
        runner,
        &git.repo_root,
        &["ls-tree", "-r", "--name-only", "HEAD"],
    ) else {
        anyhow::bail!("audit-plan requires a committed HEAD tree")
    };
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect())
}

fn area_stats(files: &[PathBuf], ignore_rules: &[PathBuf]) -> BTreeMap<String, AuditArea> {
    let mut areas = BTreeMap::new();
    for path in files {
        if matches_safe_to_ignore(path, ignore_rules) {
            continue;
        }
        let label = area_label(path);
        areas
            .entry(label.clone())
            .or_insert(AuditArea {
                label,
                files: 0,
                churn: 0,
            })
            .files += 1;
    }
    if areas.is_empty() {
        areas.insert(
            ROOT_AREA.to_string(),
            AuditArea {
                label: ROOT_AREA.to_string(),
                files: 0,
                churn: 0,
            },
        );
    }
    areas
}

fn with_churn(
    mut areas: BTreeMap<String, AuditArea>,
    history: Option<&[Vec<String>]>,
) -> Vec<AuditArea> {
    let mut churn = BTreeMap::<String, usize>::new();
    if let Some(commits) = history {
        for files in commits {
            let mut seen = BTreeSet::new();
            for file in files {
                if seen.insert(file) {
                    *churn.entry(area_label(Path::new(file))).or_default() += 1;
                }
            }
        }
    }
    for area in areas.values_mut() {
        area.churn = churn.get(&area.label).copied().unwrap_or(0);
    }
    areas.into_values().collect()
}

fn area_label(path: &Path) -> String {
    let mut components = path.components();
    let Some(first) = components.next() else {
        return ROOT_AREA.to_string();
    };
    if components.next().is_none() {
        ROOT_AREA.to_string()
    } else {
        first.as_os_str().to_string_lossy().into_owned()
    }
}

pub fn default_surface_count(areas: &[AuditArea]) -> usize {
    let limit = areas.len().clamp(1, MAX_SURFACES);
    (1..=limit)
        .find(|count| {
            pack_areas(areas.to_vec(), *count)
                .iter()
                .all(|surface| surface.files <= TARGET_FILES_PER_SURFACE)
        })
        .unwrap_or(limit)
}

pub fn pack_areas(mut areas: Vec<AuditArea>, count: usize) -> Vec<AuditSurface> {
    let count = count.clamp(1, areas.len().max(1));
    areas.sort_by(|left, right| {
        right
            .weight()
            .cmp(&left.weight())
            .then(left.label.cmp(&right.label))
    });
    let mut surfaces = (0..count)
        .map(|_| AuditSurface {
            areas: Vec::new(),
            files: 0,
            churn: 0,
        })
        .collect::<Vec<_>>();
    for area in areas {
        let index = (0..surfaces.len())
            .min_by_key(|&index| {
                (
                    surfaces[index].files + surfaces[index].churn,
                    surfaces[index].files,
                    index,
                )
            })
            .unwrap();
        surfaces[index].files += area.files;
        surfaces[index].churn += area.churn;
        surfaces[index].areas.push(area);
    }
    surfaces
}

fn prior_findings(
    runner: &impl CommandRunner,
    git: &GitState,
) -> Result<(Option<PathBuf>, Vec<PathBuf>)> {
    let Some(output) = run_git_ok(
        runner,
        &git.repo_root,
        &["ls-tree", "-r", "--name-only", "HEAD", "--", "docs/audit"],
    ) else {
        return Ok((None, Vec::new()));
    };
    let mut by_date = BTreeMap::<String, Vec<PathBuf>>::new();
    for path in output
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        let mut parts = path.split('/');
        if parts.next() == Some("docs")
            && parts.next() == Some("audit")
            && let Some(date) = parts.next()
            && is_audit_date(date)
        {
            by_date
                .entry(date.to_string())
                .or_default()
                .push(PathBuf::from(path));
        }
    }
    let Some((date, mut files)) = by_date.into_iter().next_back() else {
        return Ok((None, Vec::new()));
    };
    files.sort();
    Ok((Some(PathBuf::from(format!("docs/audit/{date}"))), files))
}

fn is_audit_date(value: &str) -> bool {
    value.len() == 10
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(label: &str, files: usize, churn: usize) -> AuditArea {
        AuditArea {
            label: label.into(),
            files,
            churn,
        }
    }

    #[test]
    fn packing_is_deterministic_and_tie_breaks_lexically() {
        let areas = vec![area("z", 2, 0), area("a", 2, 0), area("m", 1, 0)];
        assert_eq!(pack_areas(areas.clone(), 2), pack_areas(areas, 2));
        assert_eq!(
            pack_areas(vec![area("z", 1, 0), area("a", 1, 0)], 1)[0].areas[0].label,
            "a"
        );
    }

    #[test]
    fn default_count_clamps_and_exclusions_can_leave_one_area() {
        assert_eq!(default_surface_count(&[area("all", 1, 0)]), 1);
        assert_eq!(pack_areas(vec![area("all", 1, 0)], 0).len(), 1);
        assert_eq!(pack_areas(vec![area("all", 1, 0)], 999).len(), 1);
    }

    #[test]
    fn unavailable_history_leaves_file_weight_unchanged() {
        let areas = BTreeMap::from([("src".to_string(), area("src", 4, 0))]);
        let result = with_churn(areas, None);
        assert_eq!(result[0].files, 4);
        assert_eq!(result[0].churn, 0);
        assert!(history_warning().contains("file counts only"));
    }

    #[test]
    fn configured_exclusions_never_become_areas() {
        let rules = vec![PathBuf::from("vendor")];
        let files = vec![PathBuf::from("src/lib.rs"), PathBuf::from("vendor/lib.rs")];
        let areas = area_stats(&files, &rules);
        assert!(areas.contains_key("src"));
        assert!(!areas.contains_key("vendor"));
    }

    #[test]
    fn rendered_synthetic_plan_is_byte_identical() {
        let plan = AuditPlan {
            repo_root: PathBuf::from("."),
            head: Some("abc".to_string()),
            consistency: "sha256:test".to_string(),
            surfaces: pack_areas(vec![area("src", 2, 3)], 1),
            prior_findings: Vec::new(),
            findings_root: None,
            validation_plan: ValidationPlan {
                source: crate::validate::ValidationSource::Unknown,
                focused: Vec::new(),
                fast: Vec::new(),
                strict: Vec::new(),
                full: Vec::new(),
                privileged: Vec::new(),
                notes: Vec::new(),
            },
            warnings: Vec::new(),
            default_budget: 4000,
        };
        assert_eq!(
            crate::render::render_audit_plan(&plan, 4000, Some(1)),
            crate::render::render_audit_plan(&plan, 4000, Some(1))
        );
    }
}
