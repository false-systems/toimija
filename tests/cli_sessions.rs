use assert_cmd::Command;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// `sessions` is read-only awareness: it reads `.toimija/sessions/*/session.json`,
// resolves best-effort liveness, and reports. These tests drive the real binary
// against markers written to disk.

mod common;
use common::{git_available, run_git};

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

/// A marker for this test process: alive, and seen `idle` seconds ago.
fn live_marker(id: &str, intent: &str, idle: u64) -> String {
    let now = now_secs();
    serde_json::json!({
        "id": id,
        "pid": std::process::id(),
        "agent": ["codex"],
        "worktree": "/repo",
        "branch": "feat/x",
        "head": "abc123",
        "intent": intent,
        "scope": ["src/a.rs"],
        "started_at": now - idle - 5,
        "last_seen_at": now - idle,
    })
    .to_string()
}

#[test]
fn lists_a_live_marker_with_its_intent_and_scope() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_marker(temp.path(), "100-1", &live_marker("100-1", "ship it", 0));

    let value = sessions_json(temp.path());
    let session = &value["sessions"][0];
    assert_eq!(session["liveness"], "active");
    assert_eq!(session["pid_check"], "alive");
    assert_eq!(session["intent"], "ship it");
    assert_eq!(session["scope"][0], "src/a.rs");
    // The fields ohjaa reads, and no packet path: there is no packet file.
    for field in ["id", "agent", "worktree", "branch", "last_seen_at"] {
        assert!(!session[field].is_null(), "{field}: {session}");
    }
    assert!(session.get("packet").is_none(), "{session}");

    let text = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("sessions")
        .assert()
        .success();
    let text = String::from_utf8_lossy(&text.get_output().stdout).into_owned();
    assert!(text.starts_with("LIVE\n  codex  100-1\n"), "{text}");
    assert!(text.contains("    intent: ship it\n"), "{text}");
}

/// A session whose process is not provably gone but whose `run` has not
/// stamped its marker for 90 minutes is stale, and said so.
#[test]
fn a_session_not_seen_for_ninety_minutes_is_stale_and_warned() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_marker(
        temp.path(),
        "100-1",
        &live_marker("100-1", "thinking", 91 * 60),
    );

    let value = sessions_json(temp.path());
    assert_eq!(value["sessions"][0]["liveness"], "stale");
    let warnings = value["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w["message"]
            .as_str()
            .unwrap()
            .contains("has not been seen for 90 minutes")),
        "{warnings:?}"
    );
}

/// A session whose process is gone has ended: it is not listed. Gone means
/// proven: no such process, or the pid held by a process that started at
/// another time (a pid outlives its process).
#[test]
fn a_session_whose_process_is_gone_is_not_listed() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let mut gone: serde_json::Value = serde_json::from_str(&live_marker("100-1", "x", 0)).unwrap();
    gone["pid"] = serde_json::json!(2_000_000_000u32);
    write_marker(temp.path(), "100-1", &gone.to_string());
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut reused: serde_json::Value =
            serde_json::from_str(&live_marker("100-2", "y", 0)).unwrap();
        reused["pid_start"] = serde_json::json!(1);
        write_marker(temp.path(), "100-2", &reused.to_string());
    }

    let value = sessions_json(temp.path());
    assert_eq!(value["sessions"], serde_json::json!([]), "{value:#}");
    assert_eq!(value["coverage"]["status"], "available");
    let text = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("sessions")
        .assert()
        .success();
    assert_eq!(
        String::from_utf8_lossy(&text.get_output().stdout),
        "No open sessions under .toimija/sessions/.\n"
    );
    // Listing is read-only: the markers are still there.
    assert!(temp.path().join(".toimija/sessions/100-1").exists());
}

#[test]
fn a_malformed_sibling_marker_is_visible_not_silent() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_marker(temp.path(), "100-1", &live_marker("100-1", "ship it", 0));
    // A sibling whose marker is corrupt: it must degrade the scan coverage and
    // produce a warning naming the file — never shrink the listing silently.
    write_marker(temp.path(), "100-2", "{corrupt");

    let value = sessions_json(temp.path());
    assert_eq!(value["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(value["coverage"]["status"], "degraded");
    assert!(
        value["coverage"]["reason"]
            .as_str()
            .unwrap()
            .contains("1 session marker(s) unreadable")
    );
    let warnings = value["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| {
            let message = w["message"].as_str().unwrap();
            message.contains("session marker unreadable") && message.contains("100-2")
        }),
        "the malformed sibling must be named in the warnings"
    );

    // The default text listing shows the same blind spot.
    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("sessions")
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("session marker unreadable"));
    assert!(stdout.contains("100-2"));
}

#[test]
fn a_healthy_scan_reports_available_coverage() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    // Empty repo: no sessions at all is a successful empty observation.
    let value = sessions_json(temp.path());
    assert_eq!(value["sessions"].as_array().unwrap().len(), 0);
    assert_eq!(value["coverage"]["status"], "available");
}

/// `run` records when its own process started, and its session reads as live
/// while it runs: the recorded start is the one a later check reads. When the
/// agent exits, the session ends and its marker is gone.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_run_reads_as_live_while_it_runs_and_leaves_no_marker() {
    use std::os::unix::fs::PermissionsExt;
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let toimija = assert_cmd::cargo::cargo_bin("toimija");
    let agent = temp.path().join("agent");
    fs::write(
        &agent,
        format!(
            "#!/bin/sh\n'{}' sessions --json > during.json\n",
            toimija.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("TOIMIJA_SESSION")
        .args(["run", "--intent", "start time", "--"])
        .arg(&agent)
        .assert()
        .success();
    let during: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(temp.path().join("during.json")).unwrap())
            .unwrap();
    let session = &during["sessions"][0];
    assert!(session["pid_start"].as_u64().is_some(), "{session}");
    assert_eq!(session["pid_check"], "alive", "{session}");
    assert_eq!(session["liveness"], "active", "{session}");
    assert_eq!(session["intent"], "start time", "{session}");

    let id = session["id"].as_str().unwrap();
    assert!(!temp.path().join(".toimija/sessions").join(id).exists());
    assert_eq!(
        sessions_json(temp.path())["sessions"],
        serde_json::json!([])
    );
}

/// A `run` killed before it could remove its marker leaves one whose process
/// is gone. The next `run` beside it removes it; a session whose process is
/// alive, or that cannot be proven gone, is kept.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn run_removes_the_markers_of_sessions_that_ended_without_removing_them() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let mut ended: serde_json::Value = serde_json::from_str(&live_marker("100-1", "x", 0)).unwrap();
    ended["pid_start"] = serde_json::json!(1);
    write_marker(temp.path(), "100-1", &ended.to_string());
    write_marker(temp.path(), "100-2", &live_marker("100-2", "alive", 0));
    write_marker(temp.path(), "100-3", "{corrupt");

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("TOIMIJA_SESSION")
        .args(["run", "--intent", "prune", "--", "true"])
        .assert()
        .success();
    let sessions = temp.path().join(".toimija/sessions");
    assert!(!sessions.join("100-1").exists());
    assert!(sessions.join("100-2").exists());
    assert!(sessions.join("100-3").exists());
}
