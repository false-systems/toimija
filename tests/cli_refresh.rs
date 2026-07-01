use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

// `refresh` recomputes the live packet and writes `.toimija/current.md` +
// `.toimija/current.json`. It is read-only except for those two files.

mod common;
use common::{git_available, run_git};

fn init_git_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("README.md"), "# Demo\n").unwrap();
    run_git(path, &["add", "README.md"]);
    run_git(path, &["commit", "-m", "initial"]);
}

fn refresh(path: &Path) {
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(path)
        .arg("refresh")
        .assert()
        .success()
        .stdout(predicate::str::contains(".toimija/current.md"));
}

#[test]
fn refresh_writes_current_md_and_json() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    refresh(temp.path());

    let md = fs::read_to_string(temp.path().join(".toimija/current.md")).unwrap();
    assert!(md.contains("# Toimija Context"));

    let json = fs::read_to_string(temp.path().join(".toimija/current.json")).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&json).expect("current.json must be valid JSON");
    assert!(value.is_object());
    assert_eq!(value["consistency"]["algorithm"], "toimija-consistency-v2");
    assert!(
        value["consistency"]["index_fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(
        value["consistency"]["worktree_fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(
        value["consistency"]["untracked_fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(
        value["consistency"]["token"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(value["consistency"]["head"]["kind"].is_string());
    assert!(value["consistency"]["git_operation"]["kind"].is_string());
    assert!(value["consistency"]["conflict_state"]["has_conflicts"].is_boolean());
}

#[test]
fn refresh_updates_a_stale_packet_after_git_state_changes() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    refresh(temp.path());
    let first = fs::read_to_string(temp.path().join(".toimija/current.json")).unwrap();
    assert!(
        !first.contains("late_arrival.rs"),
        "new file should not be present before it exists"
    );

    // Change the working-tree state, then refresh again.
    fs::write(temp.path().join("late_arrival.rs"), "// new\n").unwrap();
    refresh(temp.path());
    let second = fs::read_to_string(temp.path().join(".toimija/current.json")).unwrap();

    assert_ne!(first, second, "packet should change when Git state changes");
    assert!(
        second.contains("late_arrival.rs"),
        "refreshed packet should reflect the new untracked file"
    );
}

#[test]
fn refresh_mutates_nothing_outside_current_files() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let readme_before = fs::read_to_string(temp.path().join("README.md")).unwrap();

    refresh(temp.path());

    // The only writes are the two live files; a pre-existing tracked file is
    // untouched and no agent docs appear.
    assert_eq!(
        readme_before,
        fs::read_to_string(temp.path().join("README.md")).unwrap()
    );
    assert!(!temp.path().join("AGENTS.md").exists());
    assert!(!temp.path().join("CLAUDE.md").exists());

    // Git sees only `.toimija/` as new — nothing tracked was modified.
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&status.stdout);
    for line in status.lines() {
        assert!(
            line.contains(".toimija/"),
            "unexpected change outside .toimija/: {line:?}"
        );
    }
}

#[test]
fn refresh_json_prints_packet_to_stdout() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["refresh", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let value: serde_json::Value =
        serde_json::from_slice(&output).expect("refresh --json must emit valid JSON");
    assert!(value.is_object());
    // And the file was still written.
    assert!(temp.path().join(".toimija/current.json").exists());
}
