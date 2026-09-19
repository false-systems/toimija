use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

mod common;
use common::{git_available, run_git};

fn init_git_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
}

fn write_marker(repo: &Path, id: &str, status: &str) {
    let dir = repo.join(".toimija/sessions").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("session.json"),
        format!(
            r#"{{
                "id": "{id}", "pid": 1, "agent": ["codex"],
                "worktree": "/repo", "branch": "main", "head": "entry",
                "intent": "issue 77", "scope": ["src/handoff.rs"],
                "started_at": 100, "status": "{status}",
                "closed_at": null, "exit_status": null
            }}"#
        ),
    )
    .unwrap();
}

#[test]
fn handoff_writes_then_show_round_trips_via_env_handshake() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_marker(temp.path(), "100-1", "active");

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("TOIMIJA_SESSION", "100-1")
        .args(["handoff", "next: run cargo test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Wrote handoff"));

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["handoff", "--show", "100-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 record(s)"))
        .stdout(predicate::str::contains("next: run cargo test"));

    let raw = fs::read_to_string(temp.path().join(".toimija/sessions/100-1/handoff.jsonl"))
        .expect("handoff should be written");
    let record: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(record["sauma"]["envelope"], "sauma-envelope.v2");
    assert_eq!(record["sauma"]["schema"], "handoff-record.v1");
    assert_eq!(record["sauma"]["producer"]["run"], "100-1");
    assert_eq!(record["sauma"]["anchored_at"], record["consistency_token"]);
}

#[test]
fn handoff_json_show_outputs_all_records_as_an_array() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_marker(temp.path(), "100-1", "active");

    for note in ["first", "second"] {
        Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(temp.path())
            .env("TOIMIJA_SESSION", "100-1")
            .args(["handoff", "--json", note])
            .assert()
            .success();
    }

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["handoff", "--show", "100-1", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let records = json.as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["next_step"], "first");
    assert_eq!(records[1]["next_step"], "second");
}

#[test]
fn handoff_outside_a_run_session_fails_with_guidance() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("TOIMIJA_SESSION")
        .arg("handoff")
        .assert()
        .failure()
        .stderr(predicate::str::contains("TOIMIJA_SESSION is not set"));
}

#[test]
fn handoff_refuses_a_closed_session() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_marker(temp.path(), "100-1", "closed");

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("TOIMIJA_SESSION", "100-1")
        .args(["handoff", "too late"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("closed"));
}

#[test]
fn handoff_show_missing_file_exits_nonzero() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["handoff", "--show", "100-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no handoff recorded for session 100-1",
        ));
}

#[test]
fn handoff_rejects_a_traversal_shaped_session_id() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("TOIMIJA_SESSION", "../../evil")
        .args(["handoff", "hostile"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid session id"));
}
