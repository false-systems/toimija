use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_git(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("README.md"), "# Demo\n").unwrap();
    run_git(path, &["add", "README.md"]);
    run_git(path, &["commit", "-m", "initial"]);
}

fn init_unborn_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
}

#[test]
fn init_creates_config_without_overwrite() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains(".toimija/repo.yaml"));
    let first = fs::read_to_string(temp.path().join(".toimija/repo.yaml")).unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("Refused to overwrite"));
    let second = fs::read_to_string(temp.path().join(".toimija/repo.yaml")).unwrap();
    assert_eq!(first, second);

    // init also seeds agent guidance files; CLAUDE.md must reference AGENTS.md
    // so `doctor` does not flag a freshly initialized repo.
    assert!(temp.path().join("AGENTS.md").exists());
    let claude = fs::read_to_string(temp.path().join("CLAUDE.md")).unwrap();
    assert!(claude.contains("AGENTS.md"));
}

#[test]
fn validate_plan_does_not_run_commands() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(
        temp.path().join("mix.exs"),
        "defmodule Demo.MixProject do\nend\n",
    )
    .unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["validate", "--plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("mix test"));
}

#[test]
fn staged_file_appears_in_context() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(temp.path().join("staged.txt"), "staged\n").unwrap();
    run_git(temp.path(), &["add", "staged.txt"]);
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("staged: 1"))
        .stdout(predicate::str::contains("staged.txt"));
}

#[test]
fn committed_branch_change_appears_in_surface_with_clean_tree() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    // Pin the base branch name so discover_base resolves it deterministically.
    run_git(temp.path(), &["branch", "-M", "main"]);
    run_git(temp.path(), &["checkout", "-b", "feature"]);
    fs::write(temp.path().join("feature.txt"), "feature\n").unwrap();
    run_git(temp.path(), &["add", "feature.txt"]);
    run_git(temp.path(), &["commit", "-m", "add feature"]);
    // Working tree is clean: the file is only visible via merge-base..HEAD.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("dirty: false"))
        .stdout(predicate::str::contains("feature.txt"))
        // Changed surface is sourced from the branch diff, and branch intent is
        // rendered because a merge-base exists.
        .stdout(predicate::str::contains(
            "source: branch diff since merge-base",
        ))
        .stdout(predicate::str::contains("## Branch intent"))
        .stdout(predicate::str::contains("files changed since merge-base:"));
}

#[test]
fn unborn_branch_reports_head_and_merge_base_unavailable() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_unborn_repo(temp.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("HEAD: unavailable"))
        .stdout(predicate::str::contains(
            "HEAD unavailable; branch has no commits yet.",
        ))
        // No HEAD and no base: branch intent is unavailable and the changed
        // surface has no source to derive from.
        .stdout(predicate::str::contains(
            "No base branch detected. Branch intent is unavailable.",
        ))
        .stdout(predicate::str::contains(
            "- unavailable: no merge-base with",
        ))
        .stdout(predicate::str::contains(
            "source: unknown; no HEAD and no local changes",
        ));
}

#[test]
fn detached_head_warns_and_renders() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    run_git(temp.path(), &["checkout", "--detach"]);
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("detached: true"))
        .stdout(predicate::str::contains(
            "Detached HEAD. Broad agent work is risky here.",
        ));
}

#[test]
fn untracked_file_warns() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(temp.path().join("scratch.txt"), "scratch\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("untracked: 1"))
        .stdout(predicate::str::contains(
            "Untracked files exist. They will not be committed unless added.",
        ));
}

#[test]
fn changed_surface_falls_back_to_working_tree_without_a_base() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    // Rename the branch away from main/master so no base candidate resolves.
    // HEAD exists but there is no merge-base, so the surface source is the
    // working tree and index, not branch intent.
    run_git(temp.path(), &["branch", "-M", "wip"]);
    fs::write(temp.path().join("scratch.txt"), "scratch\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "source: working tree and index only; merge-base unavailable",
        ))
        .stdout(predicate::str::contains(
            "No base branch detected. Branch intent is unavailable.",
        ));
}
