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
        .stdout(predicate::str::contains("staged count: 1"))
        .stdout(predicate::str::contains("staged.txt"));
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
        .stdout(predicate::str::contains("Merge-base unavailable"));
}
