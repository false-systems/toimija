use std::fmt::Write;

use crate::config::InitReport;
use crate::context::ContextPacket;
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
    writeln!(&mut output, "- dirty: {}", packet.git.dirty).unwrap();
    writeln!(
        &mut output,
        "- staged count: {}",
        packet.git.staged_files.len()
    )
    .unwrap();
    writeln!(
        &mut output,
        "- unstaged count: {}",
        packet.git.unstaged_files.len()
    )
    .unwrap();
    writeln!(
        &mut output,
        "- untracked count: {}",
        packet.git.untracked_files.len()
    )
    .unwrap();
    writeln!(
        &mut output,
        "- merge-base: {}\n",
        packet.git.merge_base.as_deref().unwrap_or("unavailable")
    )
    .unwrap();

    if packet.for_diff {
        write_diff_focus(&mut output, packet);
    }

    write_paths(&mut output, "Changed surface", &packet.changed_surface);
    writeln!(&mut output, "\nRead first:").unwrap();
    if packet.read_first.is_empty() {
        writeln!(&mut output, "- unavailable").unwrap();
    } else {
        for entry in &packet.read_first {
            writeln!(&mut output, "- {} ({})", entry.path.display(), entry.reason).unwrap();
        }
    }
    writeln!(&mut output).unwrap();
    write_paths(&mut output, "Safe to ignore", &packet.safe_to_ignore);
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

fn write_paths(output: &mut String, title: &str, items: &[std::path::PathBuf]) {
    writeln!(output, "{title}:").unwrap();
    if items.is_empty() {
        writeln!(output, "- unavailable").unwrap();
    } else {
        for item in items {
            writeln!(output, "- {}", item.display()).unwrap();
        }
    }
}

fn write_diff_focus(output: &mut String, packet: &ContextPacket) {
    writeln!(output, "Diff focus:").unwrap();
    write_changed_files(output, "Committed since base", &packet.git.branch_files);
    write_changed_files(output, "Staged", &packet.git.staged_files);
    write_changed_files(output, "Unstaged", &packet.git.unstaged_files);
    write_paths(output, "Untracked", &packet.git.untracked_files);
    write_paths(output, "Conflicts", &packet.git.conflicted_files);
    writeln!(output).unwrap();
}

fn write_changed_files(output: &mut String, title: &str, items: &[ChangedFile]) {
    writeln!(output, "{title}:").unwrap();
    if items.is_empty() {
        writeln!(output, "- unavailable").unwrap();
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
            read_first: Vec::new(),
            safe_to_ignore: Vec::new(),
            validation_plan: crate::validate::validation_plan(
                Path::new("/repo"),
                &RepoConfig::default(),
                RepoKind::Rust,
            ),
            warnings: vec!["No upstream detected.".to_string()],
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
- dirty: false
- staged count: 0
- unstaged count: 0
- untracked count: 0
- merge-base: unavailable

Changed surface:
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
- validation inferred from Rust repo shape, not proven by Sykli

Warnings:
- No upstream detected.
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
      "validation inferred from Rust repo shape, not proven by Sykli"
    ]
  },
  "warnings": [
    "No upstream detected."
  ],
  "for_diff": false,
  "default_budget": 4000
}"#;
        assert_eq!(rendered, expected);
    }
}
