use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

// `enter` is the recommended agent preflight: it refreshes the live packet
// (`.toimija/current.*`) and prints it, framed as agent entry. These lock in the
// packet rendering, the JSON form, and that it writes only the live files — never
// agent policy docs.

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
fn enter_inside_a_run_session_targets_the_session_packet() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let dir = temp.path().join(".toimija/sessions/100-1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("session.json"),
        r#"{
            "id": "100-1", "pid": 1, "agent": ["claude"],
            "worktree": "/repo", "branch": "main", "head": null,
            "intent": null, "scope": [],
            "started_at": 100, "status": "active",
            "closed_at": null, "exit_status": null
        }"#,
    )
    .unwrap();

    // A wrapped agent following the docs' `toimija enter` preflight must
    // update its own session packet (the one $TOIMIJA_LIVE points at), not
    // just the repo-level summary.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("TOIMIJA_SESSION", "100-1")
        .arg("enter")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "also written to .toimija/sessions/100-1/current.md",
        ));
    assert!(dir.join("current.md").exists());
    assert!(temp.path().join(".toimija/current.md").exists());
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
