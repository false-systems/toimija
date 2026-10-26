use std::fmt::Write;

use crate::config::InitReport;
use crate::context::{ChangedSurfaceSource, ContextPacket};
use crate::git::{ChangeStatus, ChangedFile};
use crate::landed::{LandedReport, LandedStatus};
use crate::repo::{DoctorReport, DoctorStatus, RepoKind};
use crate::validate::{ValidationPlan, ValidationSource};

pub fn render_init_report(report: &InitReport) -> String {
    let mut output = String::new();
    writeln!(&mut output, "Toimija init").unwrap();
    if report.created.is_empty() {
        writeln!(&mut output, "\nCreated: none").unwrap();
    } else {
        writeln!(&mut output, "\nCreated:").unwrap();
        for path in &report.created {
            writeln!(&mut output, "- {}", crate::path_codec::display(path)).unwrap();
        }
    }
    if !report.existing.is_empty() {
        writeln!(&mut output, "\nRefused to overwrite:").unwrap();
        for path in &report.existing {
            writeln!(&mut output, "- {}", crate::path_codec::display(path)).unwrap();
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
    let header = if report.strict {
        "Toimija doctor (strict)"
    } else {
        "Toimija doctor"
    };
    writeln!(&mut output, "{header}\n").unwrap();
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

/// Cap on differing paths listed in the landed report before summarizing the
/// remainder — keeps the output scannable on a wide branch.
const MAX_DIFFERING_SHOWN: usize = 25;

pub fn render_landed_report(report: &LandedReport) -> String {
    let mut output = String::new();
    writeln!(&mut output, "Toimija landed\n").unwrap();
    let status = match report.status {
        LandedStatus::Landed => "landed",
        LandedStatus::NotLanded => "not landed",
        LandedStatus::Partial => "partially landed",
        LandedStatus::Ambiguous => "ambiguous",
        LandedStatus::Unknown => "unknown",
    };
    writeln!(&mut output, "- base: {}", report.base).unwrap();
    let head = report.head.as_deref().unwrap_or("unknown");
    writeln!(&mut output, "- branch: {} ({head})", report.branch).unwrap();
    writeln!(&mut output, "- status: {status}").unwrap();
    writeln!(&mut output, "- reason: {}", report.reason).unwrap();
    if report.touched > 0 {
        writeln!(
            &mut output,
            "- reflected: {}/{}",
            report.reflected, report.touched
        )
        .unwrap();
    }
    if !report.differing.is_empty() {
        writeln!(&mut output, "\nNot yet reflected:").unwrap();
        for path in report.differing.iter().take(MAX_DIFFERING_SHOWN) {
            writeln!(&mut output, "- {path}").unwrap();
        }
        let remaining = report.differing.len().saturating_sub(MAX_DIFFERING_SHOWN);
        if remaining > 0 {
            writeln!(&mut output, "- … +{remaining} more").unwrap();
        }
    }
    if !report.ambiguous.is_empty() {
        writeln!(
            &mut output,
            "\nAmbiguous (Git could not prove the branch patch is reflected):"
        )
        .unwrap();
        for entry in report.ambiguous.iter().take(MAX_DIFFERING_SHOWN) {
            // The heuristic is reported as evidence, never as a landed decision.
            let evidence = if entry.line_containment {
                "added lines appear present and removed lines appear absent (textual heuristic)"
            } else {
                "added lines are not all present in base (textual heuristic)"
            };
            writeln!(&mut output, "- {} — {evidence}", entry.path).unwrap();
        }
        let remaining = report.ambiguous.len().saturating_sub(MAX_DIFFERING_SHOWN);
        if remaining > 0 {
            writeln!(&mut output, "- … +{remaining} more").unwrap();
        }
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
            writeln!(output, "- {}", crate::path_codec::display(item)).unwrap();
        }
        write_more_line(output, items.len(), MAX_LIST_SHOWN, "");
    }
}

fn write_changed_list(output: &mut String, items: &[ChangedFile], empty: &str) {
    if items.is_empty() {
        writeln!(output, "- {empty}").unwrap();
    } else {
        for item in items.iter().take(MAX_LIST_SHOWN) {
            if let Some(original) = &item.original_path {
                let similarity = item
                    .similarity
                    .map(|score| format!(" ({score}%)"))
                    .unwrap_or_default();
                writeln!(
                    output,
                    "- {} {} -> {}{}",
                    status_label(&item.status),
                    crate::path_codec::display(original),
                    crate::path_codec::display(&item.path),
                    similarity
                )
                .unwrap();
            } else {
                writeln!(
                    output,
                    "- {} {}",
                    status_label(&item.status),
                    crate::path_codec::display(&item.path)
                )
                .unwrap();
            }
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
    if let Some(gate) = &plan.gate {
        write_h3(&mut output, &format!("Gate ({})", gate.contract));
        writeln!(&mut output, "- {}", gate.command).unwrap();
        if gate.tasks.is_empty() {
            writeln!(&mut output, "- tasks: none named (see Notes)").unwrap();
        } else {
            writeln!(&mut output, "- tasks: {}", gate.tasks.join(", ")).unwrap();
        }
    }
    if !plan.focused.is_empty() {
        write_h3(&mut output, "Focused (from changed surface)");
        writeln!(
            &mut output,
            "- derived from the changed surface — a fast first pass, not a substitute for the full run"
        )
        .unwrap();
        write_str_list(&mut output, &plan.focused, "none");
    }
    // Beside a declared gate only the tiers the repo config adds are shown;
    // without one every tier renders, an empty one as `none`.
    for (title, commands) in [
        ("Fast", &plan.fast),
        ("Strict", &plan.strict),
        ("Full", &plan.full),
        ("Privileged", &plan.privileged),
    ] {
        if plan.gate.is_some() && commands.is_empty() {
            continue;
        }
        write_h3(&mut output, title);
        write_str_list(&mut output, commands, "none");
    }
    write_h3(&mut output, "Notes");
    write_str_list(&mut output, &plan.notes, "none");
    output
}

fn primary_language_evidence(kind: RepoKind) -> &'static str {
    crate::adapters::pack::for_kind(kind)
        .map(|pack| pack.root_evidence)
        .unwrap_or("no recognized root manifest")
}

pub fn render_context_markdown(packet: &ContextPacket, budget: usize) -> String {
    let mut output = String::new();
    writeln!(&mut output, "# Toimija Context").unwrap();
    if let Some(artifact_id) = &packet.artifact_id {
        writeln!(&mut output, "- artifact-id: {artifact_id}").unwrap();
    }
    // Proposal 0006: demonstrate, not just assert, that reading the packet is
    // cheaper than rediscovery. Honest counts from data already computed.
    writeln!(
        &mut output,
        "- compiled-from: {} changed file(s), {} recent commit(s) — reading this replaces that discovery",
        packet.changed_surface.len(),
        packet.git.recent_commits.len(),
    )
    .unwrap();
    if packet.for_diff {
        writeln!(
            &mut output,
            "\nMode: for-diff. Impact analysis: not yet available."
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

    write_h2(&mut output, "Languages");
    writeln!(
        &mut output,
        "- primary: {} ({})",
        packet.repo_kind.label(),
        primary_language_evidence(packet.repo_kind)
    )
    .unwrap();
    if packet.secondary_languages.is_empty() {
        writeln!(&mut output, "- also present: none").unwrap();
    } else {
        for language in packet.secondary_languages.iter().take(MAX_LIST_SHOWN) {
            writeln!(
                &mut output,
                "- also present: {} ({}; manifest: {})",
                language.kind.label(),
                crate::path_codec::display(&language.root),
                crate::path_codec::display(&language.manifest)
            )
            .unwrap();
        }
        write_more_line(
            &mut output,
            packet.secondary_languages.len(),
            MAX_LIST_SHOWN,
            "",
        );
    }

    // The session block renders only on per-session packets — the wrapped
    // agent reads its own declared identity back; repo-level packets skip it.
    if let Some(session) = &packet.session {
        write_h2(&mut output, "Session");
        writeln!(&mut output, "- id: {}", session.id).unwrap();
        writeln!(
            &mut output,
            "- intent: {}",
            session.intent.as_deref().unwrap_or("unknown")
        )
        .unwrap();
        writeln!(
            &mut output,
            "- declared scope: {}",
            if session.scope.is_empty() {
                "none".to_string()
            } else {
                session.scope.join(", ")
            }
        )
        .unwrap();
    }

    // Proposal 0005: the contract renders on every packet and early.
    write_workset_contract(&mut output);

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
    writeln!(
        &mut output,
        "- ahead/behind: {}",
        ahead_behind_label(&packet.git)
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

    write_session_drift(&mut output, packet);
    write_branch_surface(&mut output, packet);

    write_h2(&mut output, "Read first");
    let read_first = &packet.read_first;
    if read_first.is_empty() {
        writeln!(&mut output, "- none").unwrap();
    } else {
        for entry in read_first.iter().take(MAX_LIST_SHOWN) {
            writeln!(
                &mut output,
                "- {} ({})",
                crate::path_codec::display(&entry.path),
                entry.reason
            )
            .unwrap();
        }
        write_more_line(&mut output, read_first.len(), MAX_LIST_SHOWN, "");
    }

    output.push_str(&render_validation_plan(&packet.validation_plan));

    write_h2(&mut output, "Warnings");
    write_str_list(&mut output, &packet.warnings, "none");

    write_h2(&mut output, "Configured exclusions");
    // Proposal 0005 D3: an instruction, not an inventory.
    if !packet.safe_to_ignore.is_empty() {
        writeln!(&mut output, "Do not scan:").unwrap();
    }
    write_path_list(&mut output, &packet.safe_to_ignore, "none");

    write_changed_surface(&mut output, packet);

    // Optional sections are selected atomically. A budget may omit a whole
    // section, never cut a path or silently drop safety-critical state.
    let mut optional = Vec::new();
    let mut recent = String::new();
    write_recent_commits(&mut recent, packet);
    optional.push(("recent commits", packet.git.recent_commits.len(), recent));
    let mut worktree = String::new();
    if packet.for_diff {
        write_diff_focus(&mut worktree, packet);
    } else {
        write_worktree_breakdown(&mut worktree, packet);
    }
    let worktree_items = packet.git.branch_files.len()
        + packet.git.staged_files.len()
        + packet.git.unstaged_files.len()
        + packet.git.untracked_files.len()
        + packet.git.conflicted_files.len();
    optional.push(("working-tree details", worktree_items, worktree));

    append_optional_sections(&mut output, budget, optional);
    output
}

fn append_optional_sections(
    output: &mut String,
    budget: usize,
    sections: Vec<(&'static str, usize, String)>,
) {
    let mut omitted = Vec::new();
    for (name, count, section) in sections {
        if output.len() + section.len() <= budget {
            output.push_str(&section);
        } else if count > 0 {
            omitted.push((name, count));
        }
    }
    if omitted.is_empty() {
        return;
    }
    write_h2(output, "Budget omissions");
    for (name, count) in omitted {
        writeln!(output, "- {name}: {count} item(s)").unwrap();
    }
    writeln!(
        output,
        "- full evidence: use the companion JSON packet or `toimija context --json`\n\n\
         Output truncated to fit approximate budget."
    )
    .unwrap();
}

fn write_workset_contract(output: &mut String) {
    write_h2(output, "Workset contract");
    writeln!(
        output,
        "Work from this packet's Changed surface and Read first; they are your working\n\
         set, not a preamble. A session's declared scope is intent, not evidence.\n\
         Configured exclusions are do-not-scan. You MAY read outside the\n\
         workset when a concrete unresolved dependency requires it — but you MUST\n\
         declare the expansion first: `toimija intent \"<why>\" --scope <path>` (or, if\n\
         you are not under a session, state the expansion and its reason in your\n\
         visible output). Expansion is never forbidden; invisible expansion is."
    )
    .unwrap();
}

/// `<n> ahead, <m> behind <ref> (<what the ref is>)`, or why it is unknown.
fn ahead_behind_label(git: &crate::git::GitState) -> String {
    match (git.ahead, git.behind, git.ahead_behind_ref.as_deref()) {
        (Some(ahead), Some(behind), Some(against)) => {
            let kind = match git.upstream.as_deref() {
                Some(upstream) if upstream == against => "upstream",
                Some(_) => "upstream gone",
                None => "no upstream",
            };
            format!("{ahead} ahead, {behind} behind {against} ({kind})")
        }
        _ => "unknown — no upstream and no remote default branch".to_string(),
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

/// The branch surface is the observed diff since the selected merge-base, as
/// opposed to dirty state, which is only what changed in the worktree now.
fn write_branch_surface(output: &mut String, packet: &ContextPacket) {
    write_h2(output, "Branch surface");
    match packet.git.merge_base.as_deref() {
        Some(merge_base) => {
            writeln!(
                output,
                "- compared against: {}",
                packet.git.base.as_deref().unwrap_or("unavailable")
            )
            .unwrap();
            writeln!(output, "- merge-base: {merge_base}").unwrap();
            // A failed branch diff must never read as "0 files changed" —
            // render the blind spot instead of the count (R10, TA-005).
            if packet.git.branch_diff_coverage.is_available() {
                writeln!(
                    output,
                    "- files changed since merge-base: {}",
                    packet.git.branch_files.len()
                )
                .unwrap();
            } else {
                writeln!(
                    output,
                    "- files changed since merge-base: unknown — collection {}{}",
                    packet.git.branch_diff_coverage.status,
                    packet
                        .git
                        .branch_diff_coverage
                        .reason
                        .as_deref()
                        .map(|reason| format!(" ({reason})"))
                        .unwrap_or_default()
                )
                .unwrap();
            }
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

fn write_session_drift(output: &mut String, packet: &ContextPacket) {
    let Some(session) = &packet.session else {
        return;
    };
    let branch_moved = match (&session.entry_branch, &packet.git.branch) {
        (Some(entry), Some(current)) if entry != current => Some((entry, current)),
        _ => None,
    };
    let head_moved = match (&session.entry_head, &packet.git.head_oid) {
        (Some(entry), Some(current)) if entry != current => Some((entry, current)),
        _ => None,
    };
    if branch_moved.is_none() && head_moved.is_none() {
        return;
    }
    write_h2(output, "Session drift");
    if let Some((entry, current)) = branch_moved {
        writeln!(output, "- branch: {entry} → {current} (moved since entry)").unwrap();
    }
    if let Some((entry, current)) = head_moved {
        writeln!(
            output,
            "- HEAD: {} → {} (moved since entry)",
            short_oid(entry),
            short_oid(current)
        )
        .unwrap();
    }
}

fn short_oid(oid: &str) -> &str {
    &oid[..oid.len().min(7)]
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
/// reader knows whether branch-diff evidence informed it.
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
            "- {} files — see Read first",
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

fn source_label(source: &ValidationSource) -> &'static str {
    match source {
        ValidationSource::RepoConfig => "repo config",
        ValidationSource::Inferred => "inferred",
        ValidationSource::InferredAndRepoConfig => "inferred and repo config",
        ValidationSource::Sykli => "sykli.json",
        ValidationSource::SykliAndRepoConfig => "sykli.json and repo config",
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
    use crate::repo::{RepoInfo, RepoKind};

    fn packet() -> ContextPacket {
        ContextPacket {
            schema: crate::context::CONTEXT_SCHEMA.to_string(),
            artifact_id: None,
            repo: RepoInfo {
                name: "demo".to_string(),
                role: Some("tool".to_string()),
                not_roles: Vec::new(),
                important_docs: Vec::new(),
                boundaries: Vec::new(),
            },
            repo_kind: RepoKind::Rust,
            secondary_languages: Vec::new(),
            refresh_token: None,
            git: GitState {
                repo_root: PathBuf::from("/repo"),
                git_dir: None,
                branch: Some("main".to_string()),
                head_oid: Some("abc".to_string()),
                upstream: None,
                ahead: None,
                behind: None,
                ahead_behind_ref: None,
                base: None,
                merge_base: None,
                detached: false,
                dirty: false,
                staged_files: Vec::new(),
                unstaged_files: Vec::new(),
                untracked_files: Vec::new(),
                conflicted_files: Vec::new(),
                conflict_entries: Vec::new(),
                branch_files: Vec::new(),
                branch_diff_coverage: crate::coverage::Coverage::unrecorded(),
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
            consistency: crate::consistency::Consistency::fingerprint(&GitState {
                repo_root: PathBuf::from("/repo"),
                git_dir: Some(PathBuf::from("/repo/.git")),
                branch: Some("main".to_string()),
                head_oid: Some("abc".to_string()),
                upstream: None,
                ahead: None,
                behind: None,
                ahead_behind_ref: None,
                base: None,
                merge_base: None,
                detached: false,
                dirty: false,
                staged_files: Vec::new(),
                unstaged_files: Vec::new(),
                untracked_files: Vec::new(),
                conflicted_files: Vec::new(),
                conflict_entries: Vec::new(),
                branch_files: Vec::new(),
                branch_diff_coverage: crate::coverage::Coverage::unrecorded(),
                recent_commits: Vec::new(),
            }),
            session: None,
            sauma: None,
        }
    }

    #[test]
    fn markdown_includes_key_sections() {
        let rendered = render_context_markdown(&packet(), 4000);
        assert!(rendered.contains("# Toimija Context"));
        assert!(rendered.contains("## Repo"));
        assert!(rendered.contains("## Git"));
        assert!(rendered.contains("## Validation plan"));
        // Repo-level packets carry no session identity.
        assert!(!rendered.contains("## Session"));
    }

    #[test]
    fn failed_branch_diff_renders_unknown_not_zero() {
        // R10 (TA-005): a failed collection must never read as "0 files
        // changed" — the blind spot is rendered in place of the count.
        let mut failed = packet();
        failed.git.base = Some("origin/main".to_string());
        failed.git.merge_base = Some("baseoid".to_string());
        failed.git.branch_diff_coverage =
            crate::coverage::Coverage::failed("git diff", "git exited with status 128");
        let rendered = render_context_markdown(&failed, 4000);
        assert!(rendered.contains(
            "- files changed since merge-base: unknown — collection failed (git exited with status 128)"
        ));
        assert!(!rendered.contains("- files changed since merge-base: 0"));

        let mut available = packet();
        available.git.base = Some("origin/main".to_string());
        available.git.merge_base = Some("baseoid".to_string());
        available.git.branch_diff_coverage = crate::coverage::Coverage::available("git diff");
        let rendered = render_context_markdown(&available, 4000);
        assert!(rendered.contains("- files changed since merge-base: 0"));
    }

    #[test]
    fn workset_contract_renders_on_every_packet() {
        // Proposal 0005 D1: repo-level and per-session packets alike carry the
        // contract, and it renders before Git so budget truncation keeps it.
        let repo_level = render_context_markdown(&packet(), 4000);
        assert!(repo_level.contains("## Workset contract"));
        assert!(repo_level.contains("invisible expansion is."));

        let mut with_session = packet();
        with_session.session = Some(crate::context::SessionBlock {
            id: "100-1".to_string(),
            intent: None,
            scope: Vec::new(),
            execution: None,
            entry_branch: None,
            entry_head: None,
        });
        let rendered = render_context_markdown(&with_session, 4000);
        let contract = rendered.find("## Workset contract").unwrap();
        assert!(rendered.find("## Session").unwrap() < contract);
        assert!(contract < rendered.find("## Git\n").unwrap());
    }

    #[test]
    fn session_block_renders_identity_on_per_session_packets() {
        let mut packet = packet();
        packet.session = Some(crate::context::SessionBlock {
            id: "100-1".to_string(),
            intent: Some("test drive 2".to_string()),
            scope: vec!["src/a.rs".to_string()],
            execution: None,
            entry_branch: None,
            entry_head: None,
        });
        let rendered = render_context_markdown(&packet, 4000);
        assert!(rendered.contains("## Session"));
        assert!(rendered.contains("- id: 100-1"));
        assert!(rendered.contains("- intent: test drive 2"));
        assert!(rendered.contains("- declared scope: src/a.rs"));
    }

    #[test]
    fn session_drift_compares_entry_to_current_git_state() {
        let mut packet = packet();
        packet.git.branch = Some("feat/after".to_string());
        packet.git.head_oid = Some("bbbbbbb222".to_string());
        packet.session = Some(crate::context::SessionBlock {
            id: "100-1".to_string(),
            intent: Some("move safely".to_string()),
            scope: Vec::new(),
            execution: None,
            entry_branch: Some("feat/before".to_string()),
            entry_head: Some("aaaaaaa111".to_string()),
        });

        let rendered = render_context_markdown(&packet, 4000);
        assert!(rendered.contains("## Session drift"));
        assert!(rendered.contains("- branch: feat/before → feat/after (moved since entry)"));
        assert!(rendered.contains("- HEAD: aaaaaaa → bbbbbbb (moved since entry)"));

        packet.session.as_mut().unwrap().entry_branch = packet.git.branch.clone();
        packet.session.as_mut().unwrap().entry_head = packet.git.head_oid.clone();
        assert!(!render_context_markdown(&packet, 4000).contains("## Session drift"));
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
                index_entry: None,
                original_path: None,
                similarity: None,
            })
            .collect();
        packet.changed_surface = (0..40)
            .map(|i| PathBuf::from(format!("src/some/deep/module_{i:02}.rs")))
            .collect();
        // 1000 bytes of headroom past the mandatory Workset contract block,
        // which renders early by design (proposal 0005) and always survives.
        let rendered = render_context_markdown(&packet, 1600);
        assert!(rendered.contains("Output truncated"));
        assert!(
            rendered.contains("## Workset contract"),
            "contract must survive truncation"
        );
        // Validation and Warnings precede the raw lists, so they survive.
        assert!(
            rendered.contains("## Validation plan"),
            "validation must survive truncation"
        );
        assert!(
            rendered.contains("## Warnings"),
            "warnings must survive truncation"
        );
        assert!(rendered.contains("## Configured exclusions"));
        assert!(rendered.contains("## Changed surface"));
        assert!(rendered.contains("## Budget omissions"));
        assert!(rendered.contains("- working-tree details: 40 item(s)"));
        assert!(!rendered.contains("- recent commits: 0 item(s)"));
        assert!(!rendered.contains("## Working tree"));
    }

    #[test]
    fn small_budget_preserves_mandatory_context_without_fake_omissions() {
        let rendered = render_context_markdown(&packet(), 100);
        assert!(rendered.contains("## Workset contract"));
        assert!(rendered.contains("invisible expansion is."));
        assert!(!rendered.contains("## Budget omissions"));
        assert!(!rendered.contains("Output truncated"));
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
    fn markdown_read_first_lists_changed_files_tests_then_docs() {
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
        assert!(rendered.contains("- src/lib.rs (changed)"));
        assert!(rendered.contains("- tests/lib.rs (matching test heuristic)"));
        // Changed files, then their tests, then configured and important docs.
        let order: Vec<usize> = [
            "- src/lib.rs (changed)",
            "- tests/lib.rs (matching test heuristic)",
            "- docs/architecture.md (repo config)",
            "- README.md (important doc)",
        ]
        .iter()
        .map(|line| rendered.find(line).unwrap())
        .collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{rendered}");
        assert!(rendered.contains("- 2 files — see Read first"));
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
            index_entry: None,
            original_path: None,
            similarity: None,
        }];
        packet.git.staged_files = vec![ChangedFile {
            path: PathBuf::from("src/staged.rs"),
            status: ChangeStatus::Added,
            index_entry: None,
            original_path: None,
            similarity: None,
        }];
        packet.git.unstaged_files = vec![ChangedFile {
            path: PathBuf::from("src/unstaged.rs"),
            status: ChangeStatus::Deleted,
            index_entry: None,
            original_path: None,
            similarity: None,
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
- compiled-from: 0 changed file(s), 0 recent commit(s) — reading this replaces that discovery

## Repo
- name: demo
- role: tool
- not: (unset — add to .toimija/repo.yaml)
- kind: Rust

## Languages
- primary: Rust (Cargo.toml at root)
- also present: none

## Workset contract
Work from this packet's Changed surface and Read first; they are your working
set, not a preamble. A session's declared scope is intent, not evidence.
Configured exclusions are do-not-scan. You MAY read outside the
workset when a concrete unresolved dependency requires it — but you MUST
declare the expansion first: `toimija intent "<why>" --scope <path>` (or, if
you are not under a session, state the expansion and its reason in your
visible output). Expansion is never forbidden; invisible expansion is.

## Git
- branch: main
- HEAD: abc
- detached: false
- upstream: unavailable
- ahead/behind: unknown — no upstream and no remote default branch
- merge-base: unavailable
- dirty: false
- staged: 0
- unstaged: 0
- untracked: 0
- conflicts: 0

## Git state
Toimija starts from Git’s state machine: HEAD, index, working tree, branch, and merge-base.

## Branch surface
- unavailable: no merge-base with origin/main

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

## Configured exclusions
- none

## Changed surface
- source: working tree and index
- files: none

## Recent commits
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
"#;
        assert_eq!(rendered, expected);
    }

    #[test]
    fn json_context_snapshot_is_stable() {
        let rendered = serde_json::to_string_pretty(&packet()).unwrap();
        let expected = r#"{
  "schema": "toimija-context.v2",
  "repo": {
    "name": "demo",
    "role": "tool",
    "not_roles": [],
    "important_docs": [],
    "boundaries": []
  },
  "repo_kind": "Rust",
  "secondary_languages": [],
  "git": {
    "repo_root": "/repo",
    "git_dir": null,
    "branch": "main",
    "head_oid": "abc",
    "upstream": null,
    "ahead": null,
    "behind": null,
    "ahead_behind_ref": null,
    "base": null,
    "merge_base": null,
    "detached": false,
    "dirty": false,
    "staged_files": [],
    "unstaged_files": [],
    "untracked_files": [],
    "conflicted_files": [],
    "branch_files": [],
    "branch_diff_coverage": {
      "status": "degraded",
      "reason": "packet predates coverage reporting",
      "source": "unrecorded"
    },
    "recent_commits": []
  },
  "changed_surface": [],
  "changed_surface_source": "working_tree_and_index",
  "read_first": [],
  "safe_to_ignore": [],
  "validation_plan": {
    "source": "inferred",
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
  "consistency": {
    "algorithm": "toimija-consistency-v3",
    "head": {
      "kind": "normal",
      "branch": "main",
      "oid": "abc"
    },
    "index_fingerprint": "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "worktree_fingerprint": "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "untracked_fingerprint": "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "git_operation": {
      "kind": "none"
    },
    "conflict_state": {
      "has_conflicts": false,
      "files": []
    },
    "token": "sha256:c6f9744e44db11d1ae2f503664bf7ab238b51c7171703522e9fa33a9c12659c7"
  }
}"#;
        assert_eq!(rendered, expected);
    }
}
