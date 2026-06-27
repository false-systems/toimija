use std::fmt::Write;

use crate::config::InitReport;
use crate::context::{ChangedSurfaceSource, ContextPacket};
use crate::git::{ChangeStatus, ChangedFile};
use crate::relevance::RelevanceSource;
use crate::repo::{DoctorReport, DoctorStatus};
use crate::validate::{ValidationPlan, ValidationSource};

pub fn render_init_report(report: &InitReport) -> String {
    let mut output = String::new();
    writeln!(&mut output, "Toimija init").unwrap();
    if report.created.is_empty() {
        writeln!(&mut output, "\nCreated: none").unwrap();
    } else {
        writeln!(&mut output, "\nCreated:").unwrap();
        for path in &report.created {
            writeln!(&mut output, "- {}", path.display()).unwrap();
        }
    }
    if !report.existing.is_empty() {
        writeln!(&mut output, "\nRefused to overwrite:").unwrap();
        for path in &report.existing {
            writeln!(&mut output, "- {}", path.display()).unwrap();
        }
    }
    if !report.guidance.is_empty() {
        writeln!(&mut output, "\nNext steps:").unwrap();
        for step in &report.guidance {
            writeln!(&mut output, "- {step}").unwrap();
        }
    }
    output
}

pub fn render_doctor_report(report: &DoctorReport) -> String {
    let mut output = String::new();
    writeln!(&mut output, "Toimija doctor\n").unwrap();
    for check in &report.checks {
        let status = match check.status {
            DoctorStatus::Ok => "ok",
            DoctorStatus::Warning => "warn",
            DoctorStatus::Fail => "fail",
        };
        writeln!(
            &mut output,
            "- {status}: {} - {}",
            check.name, check.message
        )
        .unwrap();
    }
    output
}

/// Hint for identity fields (`role`, `not`) that come from `.toimija/repo.yaml`,
/// never from Git. When empty they are *unset* (user-fillable), not
/// *unavailable* (underivable), so they point at the file to edit instead of
/// silently degrading the packet.
const UNSET_HINT: &str = "(unset — add to .toimija/repo.yaml)";
const MAX_LIST_SHOWN: usize = 25;

/// A top-level section heading. A blank line precedes every `##` so sections
/// are visually separated; this is the single place that rule lives.
fn write_h2(output: &mut String, title: &str) {
    writeln!(output, "\n## {title}").unwrap();
}

/// A sub-section heading. No leading blank line: the `###` marker itself
/// distinguishes adjacent sub-blocks (e.g. Staged / Unstaged under Working tree).
fn write_h3(output: &mut String, title: &str) {
    writeln!(output, "### {title}").unwrap();
}

fn write_str_list(output: &mut String, items: &[String], empty: &str) {
    if items.is_empty() {
        writeln!(output, "- {empty}").unwrap();
    } else {
        for item in items.iter().take(MAX_LIST_SHOWN) {
            writeln!(output, "- {item}").unwrap();
        }
        write_more_line(output, items.len(), MAX_LIST_SHOWN, "");
    }
}

fn write_path_list(output: &mut String, items: &[std::path::PathBuf], empty: &str) {
    if items.is_empty() {
        writeln!(output, "- {empty}").unwrap();
    } else {
        for item in items.iter().take(MAX_LIST_SHOWN) {
            writeln!(output, "- {}", item.display()).unwrap();
        }
        write_more_line(output, items.len(), MAX_LIST_SHOWN, "");
    }
}

fn write_changed_list(output: &mut String, items: &[ChangedFile], empty: &str) {
    if items.is_empty() {
        writeln!(output, "- {empty}").unwrap();
    } else {
        for item in items.iter().take(MAX_LIST_SHOWN) {
            writeln!(
                output,
                "- {} {}",
                status_label(&item.status),
                item.path.display()
            )
            .unwrap();
        }
        write_more_line(output, items.len(), MAX_LIST_SHOWN, "");
    }
}

fn write_more_line(output: &mut String, total: usize, shown: usize, indent: &str) {
    if total > shown {
        writeln!(
            output,
            "{indent}- ... and {} more (truncated)",
            total - shown
        )
        .unwrap();
    }
}

pub fn render_validation_plan(plan: &ValidationPlan) -> String {
    let mut output = String::new();
    write_h2(&mut output, "Validation plan");
    writeln!(&mut output, "- source: {}", source_label(&plan.source)).unwrap();
    if !plan.focused.is_empty() {
        write_h3(&mut output, "Focused (from changed surface)");
        writeln!(
            &mut output,
            "- derived from the changed surface — a fast first pass, not a substitute for the full run"
        )
        .unwrap();
        write_str_list(&mut output, &plan.focused, "none");
    }
    write_h3(&mut output, "Fast");
    write_str_list(&mut output, &plan.fast, "none");
    write_h3(&mut output, "Strict");
    write_str_list(&mut output, &plan.strict, "none");
    write_h3(&mut output, "Full");
    write_str_list(&mut output, &plan.full, "none");
    write_h3(&mut output, "Privileged");
    write_str_list(&mut output, &plan.privileged, "none");
    write_h3(&mut output, "Notes");
    write_str_list(&mut output, &plan.notes, "none");
    output
}

pub fn render_context_markdown(packet: &ContextPacket, budget: usize) -> String {
    let mut output = String::new();
    writeln!(&mut output, "# Toimija Context").unwrap();
    if packet.for_diff {
        writeln!(
            &mut output,
            "\nMode: for-diff. Impact analysis: unavailable in v0.0."
        )
        .unwrap();
    }

    write_h2(&mut output, "Repo");
    writeln!(&mut output, "- name: {}", packet.repo.name).unwrap();
    writeln!(
        &mut output,
        "- role: {}",
        packet.repo.role.as_deref().unwrap_or(UNSET_HINT)
    )
    .unwrap();
    writeln!(
        &mut output,
        "- not: {}",
        if packet.repo.not_roles.is_empty() {
            UNSET_HINT.to_string()
        } else {
            packet.repo.not_roles.join(", ")
        }
    )
    .unwrap();
    writeln!(&mut output, "- kind: {:?}", packet.repo_kind).unwrap();

    write_h2(&mut output, "Git");
    writeln!(
        &mut output,
        "- branch: {}",
        packet.git.branch.as_deref().unwrap_or("unavailable")
    )
    .unwrap();
    writeln!(
        &mut output,
        "- HEAD: {}",
        packet.git.head_oid.as_deref().unwrap_or("unavailable")
    )
    .unwrap();
    writeln!(&mut output, "- detached: {}", packet.git.detached).unwrap();
    writeln!(
        &mut output,
        "- upstream: {}",
        packet.git.upstream.as_deref().unwrap_or("unavailable")
    )
    .unwrap();
    writeln!(&mut output, "- merge-base: {}", merge_base_label(packet)).unwrap();
    writeln!(&mut output, "- dirty: {}", packet.git.dirty).unwrap();
    writeln!(&mut output, "- staged: {}", packet.git.staged_files.len()).unwrap();
    writeln!(
        &mut output,
        "- unstaged: {}",
        packet.git.unstaged_files.len()
    )
    .unwrap();
    writeln!(
        &mut output,
        "- untracked: {}",
        packet.git.untracked_files.len()
    )
    .unwrap();
    writeln!(
        &mut output,
        "- conflicts: {}",
        packet.git.conflicted_files.len()
    )
    .unwrap();

    write_h2(&mut output, "Git state");
    writeln!(
        &mut output,
        "Toimija starts from Git’s state machine: HEAD, index, working tree, branch, and merge-base."
    )
    .unwrap();

    write_branch_intent(&mut output, packet);
    write_recent_commits(&mut output, packet);
    // Section order is also the budget-priority order: the raw changed-file
    // lists are emitted LAST so a large diff can never starve the high-value
    // sections (scope, read order, validation, warnings) under a tight budget.
    write_scope(&mut output, packet);
    write_read_order(&mut output, packet);

    write_h2(&mut output, "Read first");
    let read_first: Vec<_> = packet
        .read_first
        .iter()
        .filter(|entry| entry.reason == "repo config" || entry.reason == "important doc")
        .collect();
    if read_first.is_empty() {
        writeln!(&mut output, "- none").unwrap();
    } else {
        for entry in read_first.iter().take(MAX_LIST_SHOWN) {
            writeln!(&mut output, "- {} ({})", entry.path.display(), entry.reason).unwrap();
        }
        write_more_line(&mut output, read_first.len(), MAX_LIST_SHOWN, "");
    }

    output.push_str(&render_validation_plan(&packet.validation_plan));

    write_h2(&mut output, "Warnings");
    // Relevance warnings (e.g. shallow history) render alongside Git warnings.
    let mut warnings = packet.warnings.clone();
    warnings.extend(packet.relevance.warnings.iter().cloned());
    write_str_list(&mut output, &warnings, "none");

    write_coupling(&mut output, packet);
    write_risk(&mut output, packet);

    write_h2(&mut output, "Safe to ignore");
    write_path_list(&mut output, &packet.safe_to_ignore, "none");

    if packet.for_diff {
        write_diff_focus(&mut output, packet);
    } else {
        write_worktree_breakdown(&mut output, packet);
    }
    write_changed_surface(&mut output, packet);

    truncate_to_budget(output, budget)
}

/// Render the Coupling section: companions that are absent from the current
/// diff. Advisory; omitted when there is no signal.
fn write_coupling(output: &mut String, packet: &ContextPacket) {
    let coupling = &packet.relevance.coupling;
    if coupling.is_empty() {
        return;
    }
    write_h2(output, "Coupling");
    for hint in coupling {
        writeln!(
            output,
            "- {} changed, but {} was not — {}",
            hint.changed.display(),
            hint.missing.display(),
            hint.reason
        )
        .unwrap();
    }
}

/// Render the Risk section: changed files flagged by cheap Git-history
/// heuristics, with an advisory action. Omitted when empty.
fn write_risk(output: &mut String, packet: &ContextPacket) {
    let risk = &packet.relevance.risk;
    if risk.is_empty() {
        return;
    }
    write_h2(output, "Risk");
    for hint in risk {
        writeln!(
            output,
            "- {} — {}; {}",
            hint.path.display(),
            hint.reason,
            hint.action
        )
        .unwrap();
    }
}

/// Render the merge-base as `<base> @ <oid>` when both are known, the bare oid
/// when only the oid is known, or `unavailable`.
fn merge_base_label(packet: &ContextPacket) -> String {
    match (packet.git.merge_base.as_deref(), packet.git.base.as_deref()) {
        (Some(oid), Some(base)) => format!("{base} @ {oid}"),
        (Some(oid), None) => oid.to_string(),
        (None, _) => "unavailable".to_string(),
    }
}

/// Branch intent is what the whole branch is about (its diff since the
/// merge-base), as opposed to dirty state, which is only what changed now.
fn write_branch_intent(output: &mut String, packet: &ContextPacket) {
    write_h2(output, "Branch intent");
    match packet.git.merge_base.as_deref() {
        Some(merge_base) => {
            writeln!(
                output,
                "- compared against: {}",
                packet.git.base.as_deref().unwrap_or("unavailable")
            )
            .unwrap();
            writeln!(output, "- merge-base: {merge_base}").unwrap();
            writeln!(
                output,
                "- files changed since merge-base: {}",
                packet.git.branch_files.len()
            )
            .unwrap();
        }
        None => {
            writeln!(
                output,
                "- unavailable: no merge-base with {}",
                packet.git.base.as_deref().unwrap_or("origin/main")
            )
            .unwrap();
        }
    }
}

/// A short window of recent history (capped), so an agent sees what the branch
/// has been doing without running `git log` itself. Already collected in
/// GitState; this only renders it.
const RECENT_COMMITS_SHOWN: usize = 5;

fn write_recent_commits(output: &mut String, packet: &ContextPacket) {
    write_h2(output, "Recent commits");
    if packet.git.recent_commits.is_empty() {
        writeln!(output, "- none").unwrap();
    } else {
        for commit in packet.git.recent_commits.iter().take(RECENT_COMMITS_SHOWN) {
            writeln!(output, "- {} {}", commit.oid, commit.summary).unwrap();
        }
    }
}

/// The plain-mode breakdown that keeps staged, unstaged, and untracked
/// separate so an agent does not lose track of files it created but never
/// added. `--for-diff` renders the richer diff focus instead.
fn write_worktree_breakdown(output: &mut String, packet: &ContextPacket) {
    write_h2(output, "Working tree");
    write_h3(output, "Staged");
    write_changed_list(output, &packet.git.staged_files, "none");
    write_h3(output, "Unstaged");
    write_changed_list(output, &packet.git.unstaged_files, "none");
    write_h3(output, "Untracked");
    write_path_list(output, &packet.git.untracked_files, "none");
    write_h3(output, "Conflicts");
    write_path_list(output, &packet.git.conflicted_files, "none");
}

/// The changed surface, labelled with the source it was derived from so the
/// reader knows whether branch intent informed it.
fn write_changed_surface(output: &mut String, packet: &ContextPacket) {
    write_h2(output, "Changed surface");
    writeln!(
        output,
        "- source: {}",
        changed_surface_source_label(packet.changed_surface_source)
    )
    .unwrap();
    if packet.changed_surface.is_empty() {
        writeln!(output, "- files: none").unwrap();
    } else {
        writeln!(
            output,
            "- {} files — see Read order",
            packet.changed_surface.len()
        )
        .unwrap();
    }
}

fn changed_surface_source_label(source: ChangedSurfaceSource) -> &'static str {
    match source {
        ChangedSurfaceSource::BranchDiffSinceMergeBase => "branch diff since merge-base",
        ChangedSurfaceSource::WorkingTreeAndIndex => "working tree and index",
        ChangedSurfaceSource::Unknown => "unknown; no HEAD and no local changes",
    }
}

fn write_diff_focus(output: &mut String, packet: &ContextPacket) {
    write_h2(output, "Diff focus");
    write_h3(output, "Committed since base");
    write_changed_list(output, &packet.git.branch_files, "none");
    write_h3(output, "Staged");
    write_changed_list(output, &packet.git.staged_files, "none");
    write_h3(output, "Unstaged");
    write_changed_list(output, &packet.git.unstaged_files, "none");
    write_h3(output, "Untracked");
    write_path_list(output, &packet.git.untracked_files, "none");
    write_h3(output, "Conflicts");
    write_path_list(output, &packet.git.conflicted_files, "none");
}

fn status_label(status: &ChangeStatus) -> &'static str {
    match status {
        ChangeStatus::Added => "added",
        ChangeStatus::Modified => "modified",
        ChangeStatus::Deleted => "deleted",
        ChangeStatus::Renamed => "renamed",
        ChangeStatus::Copied => "copied",
        ChangeStatus::Untracked => "untracked",
        ChangeStatus::Unknown(_) => "unknown",
    }
}

/// Render the Scope section: the areas this branch is touching versus the
/// areas it leaves alone. Advisory, Git-derived; omitted when there is nothing
/// to say.
fn write_scope(output: &mut String, packet: &ContextPacket) {
    let Some(scope) = &packet.relevance.scope else {
        return;
    };
    if scope.in_play.is_empty() {
        return;
    }
    write_h2(output, "Scope");
    writeln!(output, "- source: {}", relevance_source_label(scope.source)).unwrap();
    let in_play = scope
        .in_play
        .iter()
        .map(|area| area.label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(output, "- in play: {in_play}").unwrap();
    if !scope.out_of_scope.is_empty() {
        let out = scope
            .out_of_scope
            .iter()
            .map(|area| area.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(output, "- out of scope this branch: {out}").unwrap();
    }
}

/// Render the Read order section: changed areas ranked by likely relevance,
/// each with a short boring reason. Numbered for the reader; paths grouped and
/// capped so it stays a digest, not a raw dump.
fn write_read_order(output: &mut String, packet: &ContextPacket) {
    let entries = &packet.relevance.read_order;
    if entries.is_empty() {
        return;
    }
    write_h2(output, "Read order");
    const PATHS_PER_ENTRY: usize = 5;
    for (index, entry) in entries.iter().enumerate() {
        let mut shown: Vec<String> = entry
            .paths
            .iter()
            .take(PATHS_PER_ENTRY)
            .map(|path| path.display().to_string())
            .collect();
        if entry.paths.len() > PATHS_PER_ENTRY {
            shown.push(format!("+{} more", entry.paths.len() - PATHS_PER_ENTRY));
        }
        writeln!(
            output,
            "{}. {} — {}",
            index + 1,
            shown.join(", "),
            entry.reason
        )
        .unwrap();
    }
}

fn relevance_source_label(source: RelevanceSource) -> &'static str {
    match source {
        RelevanceSource::MergeBaseDiff => "branch diff since merge-base",
        RelevanceSource::WorkingTreeAndIndex => "working tree and index",
        RelevanceSource::GitLog => "git log history",
        RelevanceSource::GitBlame => "git blame",
        RelevanceSource::Heuristic => "file-pattern heuristic",
        RelevanceSource::Unavailable => "unavailable",
    }
}

fn truncate_to_budget(mut output: String, budget: usize) -> String {
    if output.len() <= budget {
        return output;
    }
    let marker = "\n\nOutput truncated to fit approximate budget.\n";
    let keep = budget.saturating_sub(marker.len());
    // Cut on a line boundary at or before `keep`, never mid-line: a half-path
    // fragment reads like a real file to an agent, and a cut inside a multi-byte
    // char (—, ↔) would panic in String::truncate. '\n' is a single byte that
    // never appears inside a multi-byte UTF-8 sequence, so scanning raw bytes is
    // safe and the resulting index is always a char boundary.
    let upper = keep.min(output.len());
    let cut = output.as_bytes()[..upper]
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |index| index + 1);
    output.truncate(cut);
    output.push_str(marker);
    output
}

fn source_label(source: &ValidationSource) -> &'static str {
    match source {
        ValidationSource::RepoConfig => "repo config",
        ValidationSource::Inferred => "inferred",
        ValidationSource::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    use crate::config::RepoConfig;
    use crate::context::ContextPacket;
    use crate::git::GitState;
    use crate::relevance::RelevancePacket;
    use crate::repo::{RepoInfo, RepoKind};

    fn packet() -> ContextPacket {
        ContextPacket {
            repo: RepoInfo {
                name: "demo".to_string(),
                role: Some("tool".to_string()),
                not_roles: Vec::new(),
                important_docs: Vec::new(),
                boundaries: Vec::new(),
            },
            repo_kind: RepoKind::Rust,
            git: GitState {
                repo_root: PathBuf::from("/repo"),
                git_dir: None,
                branch: Some("main".to_string()),
                head_oid: Some("abc".to_string()),
                upstream: None,
                base: None,
                merge_base: None,
                detached: false,
                dirty: false,
                staged_files: Vec::new(),
                unstaged_files: Vec::new(),
                untracked_files: Vec::new(),
                conflicted_files: Vec::new(),
                branch_files: Vec::new(),
                recent_commits: Vec::new(),
            },
            changed_surface: Vec::new(),
            changed_surface_source: crate::context::ChangedSurfaceSource::WorkingTreeAndIndex,
            read_first: Vec::new(),
            safe_to_ignore: Vec::new(),
            validation_plan: crate::validate::validation_plan(
                Path::new("/repo"),
                &RepoConfig::default(),
                RepoKind::Rust,
            ),
            warnings: vec!["No upstream detected. Branch divergence is unavailable.".to_string()],
            for_diff: false,
            default_budget: Some(4000),
            relevance: RelevancePacket::default(),
        }
    }

    #[test]
    fn markdown_includes_key_sections() {
        let rendered = render_context_markdown(&packet(), 4000);
        assert!(rendered.contains("# Toimija Context"));
        assert!(rendered.contains("## Repo"));
        assert!(rendered.contains("## Git"));
        assert!(rendered.contains("## Validation plan"));
    }

    #[test]
    fn budget_preserves_high_value_sections_over_raw_lists() {
        let mut packet = packet();
        // A large working tree + changed surface that, in document order, would
        // starve the sections that matter most.
        packet.git.unstaged_files = (0..40)
            .map(|i| ChangedFile {
                path: PathBuf::from(format!("src/some/deep/module_{i:02}.rs")),
                status: ChangeStatus::Modified,
            })
            .collect();
        packet.changed_surface = (0..40)
            .map(|i| PathBuf::from(format!("src/some/deep/module_{i:02}.rs")))
            .collect();
        let rendered = render_context_markdown(&packet, 1000);
        assert!(rendered.contains("Output truncated"));
        // Validation and Warnings precede the raw lists, so they survive.
        assert!(
            rendered.contains("## Validation plan"),
            "validation must survive truncation"
        );
        assert!(
            rendered.contains("## Warnings"),
            "warnings must survive truncation"
        );
    }

    #[test]
    fn relevance_sections_survive_the_default_budget() {
        use crate::config::DEFAULT_BUDGET;
        use crate::relevance::{
            CouplingHint, RelevanceSource, RiskHint, RiskKind, ScopeArea, ScopeHints,
        };
        // A packet sized like a real feature branch: populated relevance plus a
        // sizable changed surface and working tree. Guards against the default
        // budget falling behind the packet (it did at 4000 — see #23).
        let mut packet = packet();
        packet.relevance.scope = Some(ScopeHints {
            in_play: vec![ScopeArea {
                label: "src".to_string(),
                paths: vec![PathBuf::from("src/a.rs")],
                reason: "1 changed file".to_string(),
            }],
            out_of_scope: vec![ScopeArea {
                label: "docs".to_string(),
                paths: Vec::new(),
                reason: "untouched by this branch".to_string(),
            }],
            source: RelevanceSource::MergeBaseDiff,
        });
        packet.relevance.coupling = (0..5)
            .map(|i| CouplingHint {
                changed: PathBuf::from(format!("apps/core/lib/very/long/module_{i}.ex")),
                missing: PathBuf::from(format!("apps/core/test/very/long/module_{i}_test.exs")),
                reason: "companion test exists but is not in this diff — confirm intentional"
                    .to_string(),
                source: RelevanceSource::GitLog,
            })
            .collect();
        packet.relevance.risk = (0..5)
            .map(|i| RiskHint {
                path: PathBuf::from(format!("apps/core/lib/risky/module_{i}.ex")),
                kind: RiskKind::HighChurn,
                reason: "high churn — 9 commits in the last 200 commits".to_string(),
                action: "hot file — keep the change small and well-tested".to_string(),
                source: RelevanceSource::GitLog,
            })
            .collect();
        packet.changed_surface = (0..30)
            .map(|i| PathBuf::from(format!("apps/core/lib/some/deep/path/module_{i:02}.ex")))
            .collect();
        packet.git.unstaged_files = (0..30)
            .map(|i| ChangedFile {
                path: PathBuf::from(format!("apps/core/lib/some/deep/path/module_{i:02}.ex")),
                status: ChangeStatus::Modified,
            })
            .collect();

        let rendered = render_context_markdown(&packet, DEFAULT_BUDGET);
        // Risk is the last relevance section; if it survives, all of them do.
        assert!(
            rendered.contains("## Coupling"),
            "coupling must survive the default budget"
        );
        assert!(
            rendered.contains("## Risk"),
            "risk must survive the default budget"
        );
    }

    #[test]
    fn budget_truncation_cuts_on_line_boundary_without_splitting_chars() {
        const MARKER: &str = "\n\nOutput truncated to fit approximate budget.\n";
        // The fixture contains multi-byte chars (the em dash in the unset hint,
        // the apostrophe in "Git’s state machine"). Sweeping budgets across them
        // exercises cuts that a raw byte truncate would split (and panic on).
        let packet = packet();
        for budget in 30..600 {
            let rendered = render_context_markdown(&packet, budget);
            if let Some(kept) = rendered.strip_suffix(MARKER) {
                assert!(
                    kept.is_empty() || kept.ends_with('\n'),
                    "kept content must end on a line boundary (no half-path) at budget {budget}"
                );
            }
        }
    }

    #[test]
    fn budget_truncation_does_not_panic() {
        let rendered = render_context_markdown(&packet(), 100);
        assert!(rendered.contains("Output truncated"));
    }

    #[test]
    fn recent_commits_render_capped_with_oid_and_summary() {
        use crate::git::RecentCommit;
        let mut packet = packet();
        packet.git.recent_commits = (0..7)
            .map(|i| RecentCommit {
                oid: format!("oid{i}"),
                summary: format!("commit {i}"),
            })
            .collect();
        let rendered = render_context_markdown(&packet, 4000);
        assert!(rendered.contains("## Recent commits"));
        assert!(rendered.contains("- oid0 commit 0"));
        // Capped at RECENT_COMMITS_SHOWN (5): the 6th and 7th are omitted.
        assert!(rendered.contains("- oid4 commit 4"));
        assert!(!rendered.contains("oid5"));
        assert!(!rendered.contains("oid6"));
    }

    #[test]
    fn unset_identity_fields_render_fillable_hint() {
        let mut packet = packet();
        packet.repo.role = None;
        packet.repo.not_roles = Vec::new();
        let rendered = render_context_markdown(&packet, 4000);
        assert!(rendered.contains("- role: (unset — add to .toimija/repo.yaml)"));
        assert!(rendered.contains("- not: (unset — add to .toimija/repo.yaml)"));
    }

    #[test]
    fn recent_commits_absent_renders_unavailable() {
        // The fixture has no commits.
        let rendered = render_context_markdown(&packet(), 4000);
        assert!(rendered.contains("## Recent commits\n- none"));
    }

    #[test]
    fn long_lists_are_capped_with_honest_remaining_count() {
        let mut packet = packet();
        packet.git.untracked_files = (0..(MAX_LIST_SHOWN + 3))
            .map(|index| PathBuf::from(format!("changed_{index:02}.txt")))
            .collect();

        let rendered = render_context_markdown(&packet, 4000);

        assert!(rendered.contains("- changed_00.txt"));
        assert!(rendered.contains("- changed_24.txt"));
        assert!(!rendered.contains("- changed_25.txt"));
        assert!(rendered.contains("- ... and 3 more (truncated)"));
    }

    #[test]
    fn markdown_consolidates_changed_file_views_without_changing_json() {
        let mut packet = packet();
        packet.changed_surface = vec![PathBuf::from("src/lib.rs"), PathBuf::from("tests/lib.rs")];
        packet.read_first = vec![
            crate::context::ReadFirstEntry {
                path: PathBuf::from("src/lib.rs"),
                reason: "changed".to_string(),
            },
            crate::context::ReadFirstEntry {
                path: PathBuf::from("tests/lib.rs"),
                reason: "matching test heuristic".to_string(),
            },
            crate::context::ReadFirstEntry {
                path: PathBuf::from("docs/architecture.md"),
                reason: "repo config".to_string(),
            },
            crate::context::ReadFirstEntry {
                path: PathBuf::from("README.md"),
                reason: "important doc".to_string(),
            },
        ];

        let rendered = render_context_markdown(&packet, 4000);
        assert!(rendered.contains("## Read first"));
        assert!(rendered.contains("- docs/architecture.md (repo config)"));
        assert!(rendered.contains("- README.md (important doc)"));
        assert!(!rendered.contains("- src/lib.rs (changed)"));
        assert!(!rendered.contains("- tests/lib.rs (matching test heuristic)"));
        assert!(rendered.contains("- 2 files — see Read order"));
        assert!(!rendered.contains("  - src/lib.rs"));

        let json = serde_json::to_value(&packet).unwrap();
        assert_eq!(json["changed_surface"].as_array().unwrap().len(), 2);
        assert_eq!(json["read_first"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn large_worktree_does_not_starve_later_sections_at_default_budget() {
        let mut packet = packet();
        packet.git.untracked_files = (0..500)
            .map(|index| PathBuf::from(format!("file_{index:03}.txt")))
            .collect();
        packet.changed_surface = packet.git.untracked_files.clone();
        packet.read_first = packet
            .changed_surface
            .iter()
            .map(|path| crate::context::ReadFirstEntry {
                path: path.clone(),
                reason: "changed".to_string(),
            })
            .collect();

        let rendered = render_context_markdown(&packet, 4000);

        assert!(rendered.contains("## Read first"));
        assert!(rendered.contains("## Validation plan"));
        assert!(rendered.contains("## Warnings"));
        assert!(rendered.contains("- ... and 475 more (truncated)"));
        assert!(!rendered.contains("Output truncated"));
    }

    #[test]
    fn for_diff_markdown_includes_diff_focus_sections() {
        let mut packet = packet();
        packet.for_diff = true;
        packet.git.branch_files = vec![ChangedFile {
            path: PathBuf::from("src/committed.rs"),
            status: ChangeStatus::Modified,
        }];
        packet.git.staged_files = vec![ChangedFile {
            path: PathBuf::from("src/staged.rs"),
            status: ChangeStatus::Added,
        }];
        packet.git.unstaged_files = vec![ChangedFile {
            path: PathBuf::from("src/unstaged.rs"),
            status: ChangeStatus::Deleted,
        }];
        packet.git.untracked_files = vec![PathBuf::from("notes.txt")];
        packet.git.conflicted_files = vec![PathBuf::from("src/conflict.rs")];

        let rendered = render_context_markdown(&packet, 4000);
        assert!(rendered.contains("## Diff focus"));
        assert!(rendered.contains("### Committed since base"));
        assert!(rendered.contains("- modified src/committed.rs"));
        assert!(rendered.contains("### Staged"));
        assert!(rendered.contains("- added src/staged.rs"));
        assert!(rendered.contains("### Unstaged"));
        assert!(rendered.contains("- deleted src/unstaged.rs"));
        assert!(rendered.contains("### Untracked"));
        assert!(rendered.contains("- notes.txt"));
        assert!(rendered.contains("### Conflicts"));
        assert!(rendered.contains("- src/conflict.rs"));
    }

    #[test]
    fn markdown_context_snapshot_is_stable() {
        let rendered = render_context_markdown(&packet(), 4000);
        let expected = r#"# Toimija Context

## Repo
- name: demo
- role: tool
- not: (unset — add to .toimija/repo.yaml)
- kind: Rust

## Git
- branch: main
- HEAD: abc
- detached: false
- upstream: unavailable
- merge-base: unavailable
- dirty: false
- staged: 0
- unstaged: 0
- untracked: 0
- conflicts: 0

## Git state
Toimija starts from Git’s state machine: HEAD, index, working tree, branch, and merge-base.

## Branch intent
- unavailable: no merge-base with origin/main

## Recent commits
- none

## Read first
- none

## Validation plan
- source: inferred
### Fast
- cargo test --workspace
### Strict
- cargo fmt --check
- cargo clippy --workspace --all-targets -- -D warnings
### Full
- cargo test --workspace
### Privileged
- none
### Notes
- Validation inferred from Rust repo shape, not proven by Sykli

## Warnings
- No upstream detected. Branch divergence is unavailable.

## Safe to ignore
- none

## Working tree
### Staged
- none
### Unstaged
- none
### Untracked
- none
### Conflicts
- none

## Changed surface
- source: working tree and index
- files: none
"#;
        assert_eq!(rendered, expected);
    }

    #[test]
    fn json_context_snapshot_is_stable() {
        let rendered = serde_json::to_string_pretty(&packet()).unwrap();
        let expected = r#"{
  "repo": {
    "name": "demo",
    "role": "tool",
    "not_roles": [],
    "important_docs": [],
    "boundaries": []
  },
  "repo_kind": "Rust",
  "git": {
    "repo_root": "/repo",
    "git_dir": null,
    "branch": "main",
    "head_oid": "abc",
    "upstream": null,
    "base": null,
    "merge_base": null,
    "detached": false,
    "dirty": false,
    "staged_files": [],
    "unstaged_files": [],
    "untracked_files": [],
    "conflicted_files": [],
    "branch_files": [],
    "recent_commits": []
  },
  "changed_surface": [],
  "changed_surface_source": "working_tree_and_index",
  "read_first": [],
  "safe_to_ignore": [],
  "validation_plan": {
    "source": "Inferred",
    "focused": [],
    "fast": [
      "cargo test --workspace"
    ],
    "strict": [
      "cargo fmt --check",
      "cargo clippy --workspace --all-targets -- -D warnings"
    ],
    "full": [
      "cargo test --workspace"
    ],
    "privileged": [],
    "notes": [
      "Validation inferred from Rust repo shape, not proven by Sykli"
    ]
  },
  "warnings": [
    "No upstream detected. Branch divergence is unavailable."
  ],
  "for_diff": false,
  "default_budget": 4000,
  "relevance": {
    "scope": null,
    "read_order": [],
    "coupling": [],
    "risk": [],
    "warnings": []
  }
}"#;
        assert_eq!(rendered, expected);
    }
}
