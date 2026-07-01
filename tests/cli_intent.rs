use assert_cmd::Command;
use std::fs;
use std::path::Path;

// `toimija intent` lets the wrapped agent update its own session marker — the
// session id comes from the TOIMIJA_SESSION env handshake, never from an
// argument. These tests drive the real binary against markers on disk.

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
                "id": "{id}", "pid": 1, "agent": ["claude"],
                "worktree": "/repo", "branch": "main", "head": null,
                "intent": null, "scope": ["src/a.rs"],
                "started_at": 100, "status": "{status}",
                "closed_at": null, "exit_status": null
            }}"#
        ),
    )
    .unwrap();
}

fn marker_json(repo: &Path, id: &str) -> serde_json::Value {
    let raw =
        fs::read_to_string(repo.join(".toimija/sessions").join(id).join("session.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn intent_updates_own_marker_via_the_env_handshake() {
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
        .args(["intent", "closing tracker issues", "--scope", "src/b.rs"])
        .assert()
        .success()
        .stdout(predicates::str::contains("closing tracker issues"));

    let marker = marker_json(temp.path(), "100-1");
    assert_eq!(marker["intent"], "closing tracker issues");
    // Launch-time scope kept, new path appended.
    assert_eq!(marker["scope"], serde_json::json!(["src/a.rs", "src/b.rs"]));
}

#[test]
fn intent_outside_a_run_session_fails_with_guidance() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("TOIMIJA_SESSION")
        .args(["intent", "orphan declaration"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("TOIMIJA_SESSION is not set"));
}

#[test]
fn intent_refuses_a_closed_session() {
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
        .args(["intent", "too late"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("closed"));
}

#[test]
fn intent_rejects_a_traversal_shaped_session_id() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    // A hostile environment must not steer the marker path out of
    // .toimija/sessions/.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("TOIMIJA_SESSION", "../../evil")
        .args(["intent", "hostile"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("invalid session id"));
}
