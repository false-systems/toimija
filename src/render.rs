use std::fmt::Write;

use crate::config::InitReport;
use crate::context::{ChangedSurfaceSource, ContextPacket};
use crate::git::{ChangeStatus, ChangedFile};
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

pub fn render_validation_plan(plan: &ValidationPlan) -> String {
    let mut output = String::new();
    writeln!(&mut output, "Validation plan\n").unwrap();
    writeln!(&mut output, "Source: {}", source_label(&plan.source)).unwrap();
    write_section(&mut output, "Fast", &plan.fast);
    write_section(&mut output, "Strict", &plan.strict);
    write_section(&mut output, "Full", &plan.full);
    write_section(&mut output, "Privileged", &plan.privileged);
    write_section(&mut output, "Notes", &plan.notes);
    output
}

pub fn render_context_markdown(packet: &ContextPacket, budget: usize) -> String {
    let mut output = String::new();
    writeln!(&mut output, "# Toimija Context\n").unwrap();
    if packet.for_diff {
        writeln!(
            &mut output,
            "Mode: for-diff. Impact analysis: unavailable in v0.0.\n"
        )
        .unwrap();
    }
    writeln!(&mut output, "Repo:").unwrap();
    writeln!(&mut output, "- name: {}", packet.repo.name).unwrap();
    writeln!(
        &mut output,
        "- role: {}",
        packet.repo.role.as_deref().unwrap_or("unavailable")
    )
    .unwrap();
    writeln!(
        &mut output,
        "- not: {}",
        if packet.repo.not_roles.is_empty() {
            "unavailable".to_string()
        } else {
            packet.repo.not_roles.join(", ")
        }
    )
    .unwrap();
    writeln!(&mut output, "- kind: {:?}\n", packet.repo_kind).unwrap();

    writeln!(&mut output, "Git:").unwrap();
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
        "- conflicts: {}\n",
        packet.git.conflicted_files.len()
    )
    .unwrap();

    writeln!(&mut output, "Git state:").unwrap();
    writeln!(
        &mut output,
        "Toimija starts from Git’s state machine: HEAD, index, working tree, branch, and merge-base.\n"
    )
    .unwrap();

    write_branch_intent(&mut output, packet);

    write_recent_commits(&mut output, packet);

    if packet.for_diff {
        write_diff_focus(&mut output, packet);
    } else {
        write_worktree_breakdown(&mut output, packet);
    }

    write_changed_surface(&mut output, packet);
    writeln!(&mut output, "\nRead first:").unwrap();
    if packet.read_first.is_empty() {
        writeln!(&mut output, "- unavailable").unwrap();
    } else {
        for entry in &packet.read_first {
            writeln!(&mut output, "- {} ({})", entry.path.display(), entry.reason).unwrap();
        }
    }
    writeln!(&mut output).unwrap();
    write_paths(
        &mut output,
        "Safe to ignore",
        &packet.safe_to_ignore,
        "unavailable",
    );
    writeln!(&mut output).unwrap();
    output.push_str(&render_validation_plan(&packet.validation_plan));
    write_section(&mut output, "Warnings", &packet.warnings);

    truncate_to_budget(output, budget)
}

fn write_section(output: &mut String, title: &str, items: &[String]) {
    writeln!(output, "\n{title}:").unwrap();
    if items.is_empty() {
        writeln!(output, "- unavailable").unwrap();
    } else {
        for item in items {
            writeln!(output, "- {item}").unwrap();
        }
    }
}

fn write_paths(output: &mut String, title: &str, items: &[std::path::PathBuf], empty: &str) {
    writeln!(output, "{title}:").unwrap();
    if items.is_empty() {
        writeln!(output, "- {empty}").unwrap();
    } else {
        for item in items {
            writeln!(output, "- {}", item.display()).unwrap();
        }
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
    writeln!(output, "Branch intent:").unwrap();
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
    writeln!(output).unwrap();
}

/// A short window of recent history (capped), so an agent sees what the branch
/// has been doing without running `git log` itself. Already collected in
/// GitState; this only renders it.
const RECENT_COMMITS_SHOWN: usize = 5;

fn write_recent_commits(output: &mut String, packet: &ContextPacket) {
    writeln!(output, "Recent commits:").unwrap();
    if packet.git.recent_commits.is_empty() {
        writeln!(output, "- unavailable").unwrap();
    } else {
        for commit in packet.git.recent_commits.iter().take(RECENT_COMMITS_SHOWN) {
            writeln!(output, "- {} {}", commit.oid, commit.summary).unwrap();
        }
    }
    writeln!(output).unwrap();
}

/// The plain-mode breakdown that keeps staged, unstaged, and untracked
/// separate so an agent does not lose track of files it created but never
/// added. `--for-diff` renders the richer diff focus instead.
fn write_worktree_breakdown(output: &mut String, packet: &ContextPacket) {
    writeln!(output, "Working tree:").unwrap();
    write_changed_files(output, "Staged", &packet.git.staged_files, "none");
    write_changed_files(output, "Unstaged", &packet.git.unstaged_files, "none");
    write_paths(output, "Untracked", &packet.git.untracked_files, "none");
    write_paths(output, "Conflicts", &packet.git.conflicted_files, "none");
    writeln!(output).unwrap();
}

/// The changed surface, labelled with the source it was derived from so the
/// reader knows whether branch intent informed it.
fn write_changed_surface(output: &mut String, packet: &ContextPacket) {
    writeln!(output, "Changed surface:").unwrap();
    writeln!(
        output,
        "source: {}",
        changed_surface_source_label(packet.changed_surface_source)
    )
    .unwrap();
    if packet.changed_surface.is_empty() {
        writeln!(output, "- unavailable").unwrap();
    } else {
        for item in &packet.changed_surface {
            writeln!(output, "- {}", item.display()).unwrap();
        }
    }
}

fn changed_surface_source_label(source: ChangedSurfaceSource) -> &'static str {
    match source {
        ChangedSurfaceSource::BranchDiffSinceMergeBase => "branch diff since merge-base",
        ChangedSurfaceSource::WorkingTreeAndIndex => {
            "working tree and index only; merge-base unavailable"
        }
        ChangedSurfaceSource::Unknown => "unknown; no HEAD and no local changes",
    }
}

fn write_diff_focus(output: &mut String, packet: &ContextPacket) {
    writeln!(output, "Diff focus:").unwrap();
    write_changed_files(
        output,
        "Committed since base",
        &packet.git.branch_files,
        "none",
    );
    write_changed_files(output, "Staged", &packet.git.staged_files, "none");
    write_changed_files(output, "Unstaged", &packet.git.unstaged_files, "none");
    write_paths(output, "Untracked", &packet.git.untracked_files, "none");
    write_paths(output, "Conflicts", &packet.git.conflicted_files, "none");
    writeln!(output).unwrap();
}

fn write_changed_files(output: &mut String, title: &str, items: &[ChangedFile], empty: &str) {
    writeln!(output, "{title}:").unwrap();
    if items.is_empty() {
        writeln!(output, "- {empty}").unwrap();
    } else {
        for item in items {
            writeln!(
                output,
                "- {} {}",
                status_label(&item.status),
                item.path.display()
            )
            .unwrap();
        }
    }
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

fn truncate_to_budget(mut output: String, budget: usize) -> String {
    if output.len() <= budget {
        return output;
    }
    let marker = "\n\nOutput truncated to fit approximate budget.\n";
    let keep = budget.saturating_sub(marker.len()).max(1);
    output.truncate(keep);
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
        }
    }

    #[test]
    fn markdown_includes_key_sections() {
        let rendered = render_context_markdown(&packet(), 4000);
        assert!(rendered.contains("# Toimija Context"));
        assert!(rendered.contains("Repo:"));
        assert!(rendered.contains("Git:"));
        assert!(rendered.contains("Validation plan"));
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
        assert!(rendered.contains("Recent commits:"));
        assert!(rendered.contains("- oid0 commit 0"));
        // Capped at RECENT_COMMITS_SHOWN (5): the 6th and 7th are omitted.
        assert!(rendered.contains("- oid4 commit 4"));
        assert!(!rendered.contains("oid5"));
        assert!(!rendered.contains("oid6"));
    }

    #[test]
    fn recent_commits_absent_renders_unavailable() {
        // The fixture has no commits.
        let rendered = render_context_markdown(&packet(), 4000);
        assert!(rendered.contains("Recent commits:\n- unavailable"));
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
        assert!(rendered.contains("Diff focus:"));
        assert!(rendered.contains("Committed since base:"));
        assert!(rendered.contains("- modified src/committed.rs"));
        assert!(rendered.contains("Staged:"));
        assert!(rendered.contains("- added src/staged.rs"));
        assert!(rendered.contains("Unstaged:"));
        assert!(rendered.contains("- deleted src/unstaged.rs"));
        assert!(rendered.contains("Untracked:"));
        assert!(rendered.contains("- notes.txt"));
        assert!(rendered.contains("Conflicts:"));
        assert!(rendered.contains("- src/conflict.rs"));
    }

    #[test]
    fn markdown_context_snapshot_is_stable() {
        let rendered = render_context_markdown(&packet(), 4000);
        let expected = r#"# Toimija Context

Repo:
- name: demo
- role: tool
- not: unavailable
- kind: Rust

Git:
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

Git state:
Toimija starts from Git’s state machine: HEAD, index, working tree, branch, and merge-base.

Branch intent:
- unavailable: no merge-base with origin/main

Recent commits:
- unavailable

Working tree:
Staged:
- none
Unstaged:
- none
Untracked:
- none
Conflicts:
- none

Changed surface:
source: working tree and index only; merge-base unavailable
- unavailable

Read first:
- unavailable

Safe to ignore:
- unavailable

Validation plan

Source: inferred

Fast:
- cargo test --workspace

Strict:
- cargo fmt --check
- cargo clippy --workspace --all-targets -- -D warnings

Full:
- cargo test --workspace

Privileged:
- unavailable

Notes:
- Validation inferred from Rust repo shape, not proven by Sykli

Warnings:
- No upstream detected. Branch divergence is unavailable.
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
  "default_budget": 4000
}"#;
        assert_eq!(rendered, expected);
    }
}
