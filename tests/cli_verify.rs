use assert_cmd::Command;
use std::fs;
use std::path::Path;

// `verify` is the verify-before-read gate: it checks `.toimija/current.json`
// against the live repo and exits non-zero (with reasons) when the packet is
// stale. Read-only; runs no validation, touches no network.

mod common;
use common::{git_available, run_git};

fn init_repo_with_commit(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("a.txt"), "x\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "init"]);
}

fn toimija(path: &Path) -> Command {
    let mut command = Command::cargo_bin("toimija").unwrap();
    command.current_dir(path);
    command
}

#[test]
fn fresh_packet_verifies_consistent_then_a_change_makes_it_stale() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());

    // refresh writes current.json (with the consistency block).
    toimija(temp.path()).arg("refresh").assert().success();

    // Nothing changed since the refresh -> consistent, exit 0.
    toimija(temp.path()).arg("verify").assert().success();

    // Touch the working tree -> stale, exit non-zero, with a reason in JSON.
    fs::write(temp.path().join("b.txt"), "new\n").unwrap();
    let assert = toimija(temp.path())
        .args(["verify", "--json"])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["consistent"], false);
    assert!(
        value["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str().unwrap().contains("working tree")),
        "{stdout}"
    );
}

#[test]
fn verify_without_a_packet_is_stale() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    // No refresh has run, so there is no packet to trust.
    toimija(temp.path()).arg("verify").assert().failure();
}
