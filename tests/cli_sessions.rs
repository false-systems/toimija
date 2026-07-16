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

fn write_handoff(repo: &Path, id: &str, line: &str) {
    let dir = repo.join(".toimija/sessions").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("handoff.jsonl"), line).unwrap();
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
    assert_eq!(session["handoffs"], 0);
}

#[test]
fn sessions_surfaces_handoff_counts_in_text_and_json() {
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
            "head": null,
            "intent": "ship it",
            "scope": [],
            "started_at": 100,
            "status": "closed",
            "closed_at": 200,
            "exit_status": 0
        }"#,
    );
    let record = r#"{"version":1,"session_id":"100-1","written_at":1,"agent":[],"agent_total":0,"intent":null,"branch":null,"entry_head":null,"head":null,"merge_base":null,"staged":[],"staged_total":0,"unstaged":[],"unstaged_total":0,"untracked":[],"untracked_total":0,"conflicts":[],"conflicts_total":0,"branch_scope":[],"branch_scope_total":0,"suggested_validation":[],"suggested_validation_total":0,"coupling":[],"coupling_total":0,"risk":[],"risk_total":0,"consistency_token":"t","next_step":null}"#;
    write_handoff(temp.path(), "100-1", &format!("{record}\n{record}\n"));

    let value = sessions_json(temp.path());
    assert_eq!(value["sessions"][0]["handoffs"], 2);

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("sessions")
        .assert()
        .success()
        .stdout(predicates::str::contains("handoff: 2 record(s)"));
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
fn prune_removes_old_closed_sessions_and_keeps_live_and_recent_ones() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let now = now_secs();
    let eight_days = 8 * 24 * 60 * 60;

    // Closed 8 days ago → prunable.
    write_marker(
        temp.path(),
        "old-closed",
        &format!(
            r#"{{
                "id": "old-closed", "pid": 1, "agent": ["codex"],
                "worktree": "/repo", "branch": "feat/x", "head": null,
                "intent": null, "scope": [],
                "started_at": {start}, "status": "closed",
                "closed_at": {closed}, "exit_status": 0
            }}"#,
            start = now - eight_days - 100,
            closed = now - eight_days,
        ),
    );
    // Closed an hour ago → inside retention, kept.
    write_marker(
        temp.path(),
        "fresh-closed",
        &format!(
            r#"{{
                "id": "fresh-closed", "pid": 1, "agent": ["codex"],
                "worktree": "/repo", "branch": "feat/x", "head": null,
                "intent": null, "scope": [],
                "started_at": {start}, "status": "closed",
                "closed_at": {closed}, "exit_status": 0
            }}"#,
            start = now - 7200,
            closed = now - 3600,
        ),
    );
    // Live session (this test's pid, recent start) → never touched.
    let pid = std::process::id();
    write_marker(
        temp.path(),
        "live",
        &format!(
            r#"{{
                "id": "live", "pid": {pid}, "agent": ["claude"],
                "worktree": "/repo", "branch": "feat/x", "head": null,
                "intent": "live work", "scope": [],
                "started_at": {start}, "status": "active",
                "closed_at": null, "exit_status": null
            }}"#,
            start = now - 5,
        ),
    );

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["sessions", "--prune", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["removed"], serde_json::json!(["old-closed"]));
    assert_eq!(report["kept"], 2);

    let sessions_dir = temp.path().join(".toimija/sessions");
    assert!(!sessions_dir.join("old-closed").exists());
    assert!(sessions_dir.join("fresh-closed").exists());
    assert!(sessions_dir.join("live").exists());
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

#[test]
fn a_malformed_sibling_marker_is_visible_not_silent() {
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
            "head": null,
            "intent": "ship it",
            "scope": [],
            "started_at": 100,
            "status": "closed",
            "closed_at": 200,
            "exit_status": 0
        }"#,
    );
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
