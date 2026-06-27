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

#[test]
fn validate_without_plan_is_rejected() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("validate")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "`toimija validate` only supports `--plan`",
        ));
}

#[test]
fn validate_plan_uses_repo_config_when_present() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: demo\nvalidation:\n  fast:\n    - just test-fast\n  strict:\n    - just test-strict\n  full:\n    - just test-full\n  privileged:\n    - just test-privileged\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["validate", "--plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("source: repo config"))
        .stdout(predicate::str::contains("just test-fast"))
        .stdout(predicate::str::contains("just test-strict"))
        .stdout(predicate::str::contains("just test-full"))
        .stdout(predicate::str::contains("just test-privileged"))
        .stdout(predicate::str::contains("cargo test").not())
        .stdout(predicate::str::contains("mix test").not());
}

#[test]
fn validate_plan_infers_rust_commands() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["validate", "--plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("source: inferred"))
        .stdout(predicate::str::contains("cargo test --workspace"))
        .stdout(predicate::str::contains("cargo fmt --check"))
        .stdout(predicate::str::contains(
            "Validation inferred from Rust repo shape, not proven by Sykli",
        ));
}

#[test]
fn validate_plan_for_generic_repo_is_unknown() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["validate", "--plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("source: unknown"))
        .stdout(predicate::str::contains(
            "validation unavailable for generic repo without repo config",
        ));
}

#[test]
fn validate_plan_outside_git_repo_fails() {
    let temp = tempfile::tempdir().unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["validate", "--plan"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a Git repository"));
}
