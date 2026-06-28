use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

// `run -- <command>` enters (refresh + print), watches while the command runs,
// then exits with the command's status. It never executes the validation plan.

fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_git(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
}

fn init_git_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("a.txt"), "x\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "init"]);
}

#[test]
fn run_exits_with_the_wrapped_command_status() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    // Non-zero status is propagated verbatim.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--", "sh", "-c", "exit 7"])
        .assert()
        .code(7);

    // Success is propagated too.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--", "sh", "-c", "exit 0"])
        .assert()
        .code(0);
}

#[test]
fn run_enters_then_launches_the_command() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--", "sh", "-c", "echo hi > ran.txt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Toimija agent entry"));

    // The command ran ...
    assert_eq!(
        fs::read_to_string(temp.path().join("ran.txt"))
            .unwrap()
            .trim(),
        "hi"
    );
    // ... and entry refreshed the live packet.
    assert!(temp.path().join(".toimija/current.md").exists());
    assert!(temp.path().join(".toimija/current.json").exists());
}

#[test]
fn run_does_not_execute_the_validation_plan() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    // A validation tier that would leave a marker IF Toimija ever ran it.
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test\nvalidation:\n  fast:\n    - \"touch VALIDATION_RAN\"\nboundaries:\n  - stay in scope\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--", "true"])
        .assert()
        .success();

    assert!(
        !temp.path().join("VALIDATION_RAN").exists(),
        "run must never execute the configured validation plan"
    );
    // It still entered (refreshed the live packet).
    assert!(temp.path().join(".toimija/current.json").exists());
}
