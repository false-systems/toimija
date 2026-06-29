use assert_cmd::Command;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// `sessions` is read-only awareness: it reads `.toimija/sessions/*/session.json`,
// resolves best-effort liveness, and reports. These tests drive the real binary
// against markers written to disk.

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
    assert!(output.status.success(), "git {args:?} failed");
}

fn init_git_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
}

fn write_marker(repo: &Path, id: &str, json: &str) {
    let dir = repo.join(".toimija/sessions").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("session.json"), json).unwrap();
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn sessions_json(repo: &Path) -> serde_json::Value {
    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(repo)
        .args(["sessions", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str(&stdout).expect("sessions --json must emit valid JSON")
}

#[test]
fn lists_a_closed_marker_with_its_intent_and_scope() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_marker(
        temp.path(),
        "100-1",
        r#"{
            "id": "100-1",
            "pid": 1,
            "agent": ["codex"],
            "worktree": "/repo",
            "branch": "feat/x",
            "head": "abc123",
            "intent": "ship it",
            "scope": ["src/a.rs"],
            "started_at": 100,
            "status": "closed",
            "closed_at": 200,
            "exit_status": 0
        }"#,
    );

    let value = sessions_json(temp.path());
    let session = &value["sessions"][0];
    assert_eq!(session["liveness"], "closed");
    assert_eq!(session["intent"], "ship it");
    assert_eq!(session["scope"][0], "src/a.rs");
    assert_eq!(session["packet"], ".toimija/sessions/100-1/current.md");
}

#[test]
fn an_open_marker_with_a_live_pid_is_active() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    // This test process is certainly alive; a recent start keeps it inside the
    // staleness window.
    let pid = std::process::id();
    let started = now_secs().saturating_sub(5);
    write_marker(
        temp.path(),
        &format!("{started}-{pid}"),
        &format!(
            r#"{{
                "id": "{started}-{pid}",
                "pid": {pid},
                "agent": ["claude"],
                "worktree": "/repo",
                "branch": "feat/x",
                "head": null,
                "intent": "live work",
                "scope": [],
                "started_at": {started},
                "status": "active",
                "closed_at": null,
                "exit_status": null
            }}"#
        ),
    );

    let value = sessions_json(temp.path());
    assert_eq!(value["sessions"][0]["liveness"], "active");
}

#[test]
fn an_open_marker_with_a_dead_pid_is_stale_and_warned() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    // A pid far above any real process: `kill -0` fails, so liveness is stale.
    let started = now_secs().saturating_sub(5);
    write_marker(
        temp.path(),
        "dead",
        &format!(
            r#"{{
                "id": "dead",
                "pid": 2000000000,
                "agent": ["codex"],
                "worktree": "/repo",
                "branch": "feat/x",
                "head": null,
                "intent": "abandoned",
                "scope": [],
                "started_at": {started},
                "status": "active",
                "closed_at": null,
                "exit_status": null
            }}"#
        ),
    );

    let value = sessions_json(temp.path());
    assert_eq!(value["sessions"][0]["liveness"], "stale");
    let warnings = value["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w["message"].as_str().unwrap().contains("looks stale")),
        "a stale-but-open session should produce a descriptive warning"
    );
}
