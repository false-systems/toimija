use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

// `enter` is the recommended agent preflight: it refreshes the live packet
// (`.toimija/current.*`) and prints it, framed as agent entry. These lock in the
// packet rendering, the JSON form, and that it writes only the live files — never
// agent policy docs.

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

fn init_git_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("README.md"), "# Demo\n").unwrap();
    run_git(path, &["add", "README.md"]);
    run_git(path, &["commit", "-m", "initial"]);
}

#[test]
fn enter_renders_the_context_packet() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["enter", "--for-diff", "--budget", "4000"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Toimija Context"))
        .stdout(predicate::str::contains("## Git"))
        .stdout(predicate::str::contains("## Validation plan"));
}

#[test]
fn enter_json_is_valid() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["enter", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let value: serde_json::Value =
        serde_json::from_slice(&output).expect("enter --json must emit valid JSON");
    assert!(value.is_object(), "packet should be a JSON object");
}

#[test]
fn enter_writes_the_live_packet_but_not_agent_docs() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("enter")
        .assert()
        .success()
        .stdout(predicate::str::contains("Toimija agent entry"));

    // enter refreshes the live packet ...
    assert!(temp.path().join(".toimija/current.md").exists());
    assert!(temp.path().join(".toimija/current.json").exists());
    // ... but never writes human-owned agent policy.
    assert!(!temp.path().join("AGENTS.md").exists());
    assert!(!temp.path().join("CLAUDE.md").exists());
}
