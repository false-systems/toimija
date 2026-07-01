use assert_cmd::Command;
use std::fs;
use std::path::Path;

// `verify` is the verify-before-read gate: it checks `.toimija/current.json`
// against the live repo and exits non-zero (with reasons) when the packet is
// stale. Read-only; runs no validation, touches no network.

mod common;
use common::{git_at, git_available, run_git};

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
    let ok = toimija(temp.path())
        .args(["current", "--verify", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ok: serde_json::Value = serde_json::from_slice(&ok).unwrap();
    assert_eq!(ok["status"], "verified");
    assert_eq!(ok["algorithm"], "toimija-consistency-v2");
    assert!(ok["token"].as_str().unwrap().starts_with("sha256:"));

    // Touch the working tree -> stale, exit non-zero, with a reason in JSON.
    fs::write(temp.path().join("b.txt"), "new\n").unwrap();
    let assert = toimija(temp.path())
        .args(["verify", "--json"])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["status"], "stale_packet");
    assert!(
        value["packet_token"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(
        value["current_token"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
}

#[test]
fn verify_without_a_packet_is_missing_packet() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    // No refresh has run, so there is no packet to trust.
    let assert = toimija(temp.path())
        .args(["current", "--verify", "--json"])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["status"], "missing_packet");
}

#[test]
fn verify_distinguishes_missing_and_unsupported_consistency() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(temp.path().join(".toimija/current.json"), "{}").unwrap();
    let assert = toimija(temp.path())
        .args(["current", "--verify", "--json"])
        .assert()
        .failure();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "missing_consistency");

    toimija(temp.path())
        .args(["current", "--write"])
        .assert()
        .success();
    let path = temp.path().join(".toimija/current.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    value["consistency"]["algorithm"] = serde_json::json!("other");
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    let assert = toimija(temp.path())
        .args(["current", "--verify", "--json"])
        .assert()
        .failure();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "unsupported_consistency_algorithm");
}

#[test]
fn staged_and_deleted_and_renamed_changes_are_stale() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    toimija(temp.path())
        .args(["current", "--write"])
        .assert()
        .success();

    fs::write(temp.path().join("a.txt"), "changed\n").unwrap();
    run_git(temp.path(), &["add", "a.txt"]);
    assert_stale(temp.path());

    run_git(temp.path(), &["reset", "--hard", "HEAD"]);
    toimija(temp.path())
        .args(["current", "--write"])
        .assert()
        .success();
    run_git(temp.path(), &["rm", "a.txt"]);
    assert_stale(temp.path());

    run_git(temp.path(), &["reset", "--hard", "HEAD"]);
    toimija(temp.path())
        .args(["current", "--write"])
        .assert()
        .success();
    run_git(temp.path(), &["mv", "a.txt", "renamed.txt"]);
    assert_stale(temp.path());
}

#[test]
fn staged_index_content_change_is_stale_even_when_worktree_bytes_do_not_change() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let blobs = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());

    fs::write(temp.path().join("a.txt"), "ONE\n\nTWO\n").unwrap();
    fs::write(blobs.path().join("index1.txt"), "x\n\nTWO\n").unwrap();
    let index1 = git_stdout(
        temp.path(),
        &[
            "hash-object",
            "-w",
            blobs.path().join("index1.txt").to_str().unwrap(),
        ],
    );
    run_git(
        temp.path(),
        &["update-index", "--cacheinfo", "100644", &index1, "a.txt"],
    );
    toimija(temp.path())
        .args(["current", "--write"])
        .assert()
        .success();

    fs::write(blobs.path().join("index2.txt"), "ONE\n\nx\n").unwrap();
    let index2 = git_stdout(
        temp.path(),
        &[
            "hash-object",
            "-w",
            blobs.path().join("index2.txt").to_str().unwrap(),
        ],
    );
    run_git(
        temp.path(),
        &["update-index", "--cacheinfo", "100644", &index2, "a.txt"],
    );

    assert_stale(temp.path());
}

#[test]
fn ignored_file_does_not_change_token() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    fs::write(temp.path().join(".gitignore"), "ignored.log\n").unwrap();
    run_git(temp.path(), &["add", ".gitignore"]);
    run_git(temp.path(), &["commit", "-m", "ignore"]);
    toimija(temp.path())
        .args(["current", "--write"])
        .assert()
        .success();

    fs::write(temp.path().join("ignored.log"), "ignored\n").unwrap();
    toimija(temp.path())
        .args(["current", "--verify", "--json"])
        .assert()
        .success();
}

fn git_stdout(path: &Path, args: &[&str]) -> String {
    let output = git_at(path).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn assert_stale(path: &Path) {
    let assert = toimija(path)
        .args(["current", "--verify", "--json"])
        .assert()
        .failure();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "stale_packet");
}
