use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

// `run -- <command>` enters (refresh + print), watches while the command runs,
// then exits with the command's status. It never executes the validation plan.

mod common;
use common::{git_available, run_git};

fn init_git_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("a.txt"), "x\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "init"]);
}

#[test]
fn run_exits_with_the_wrapped_command_status() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    // Non-zero status is propagated verbatim.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--", "sh", "-c", "exit 7"])
        .assert()
        .code(7);

    // Success is propagated too.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--", "sh", "-c", "exit 0"])
        .assert()
        .code(0);
}

#[cfg(unix)]
#[test]
fn run_preserves_signal_termination_as_128_plus_signal() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    // A child killed by SIGTERM (15) should surface as 128 + 15 = 143, so
    // termination is distinguishable from a generic failure (exit 1).
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--", "sh", "-c", "kill -TERM $$"])
        .assert()
        .code(143);
}

#[test]
fn run_enters_then_launches_the_command() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--", "sh", "-c", "echo hi > ran.txt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Toimija agent entry"));

    // The command ran ...
    assert_eq!(
        fs::read_to_string(temp.path().join("ran.txt"))
            .unwrap()
            .trim(),
        "hi"
    );
    // ... and entry refreshed the live packet.
    assert!(temp.path().join(".toimija/current.md").exists());
    assert!(temp.path().join(".toimija/current.json").exists());
}

#[test]
fn run_writes_a_per_session_packet_and_closes_the_marker() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--", "true"])
        .assert()
        .success();

    // Exactly one session directory, with its own packet and a closed marker.
    let sessions = temp.path().join(".toimija/sessions");
    assert!(sessions.is_dir(), "sessions dir should exist");
    let dirs: Vec<_> = fs::read_dir(&sessions)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect();
    assert_eq!(dirs.len(), 1, "exactly one session dir expected");
    let session_dir = &dirs[0];
    assert!(session_dir.join("current.md").exists());
    assert!(session_dir.join("current.json").exists());

    let marker = fs::read_to_string(session_dir.join("session.json")).unwrap();
    assert!(
        marker.contains("\"status\": \"closed\""),
        "marker should be auto-closed on exit: {marker}"
    );
    assert!(
        marker.contains("\"agent\""),
        "marker records the agent argv"
    );

    // The repo-level summary is still written (demoted, last-writer-wins).
    assert!(temp.path().join(".toimija/current.md").exists());
}

#[test]
fn run_intent_reaches_the_session_packet() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    // A wrapped harness infers intent from the packet, so the launch-time
    // declaration must land there — not only on the marker (issue #84).
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "test drive 2",
            "--scope",
            "src/a.rs",
            "--",
            "true",
        ])
        .assert()
        .success();

    let sessions = temp.path().join(".toimija/sessions");
    let session_dir = fs::read_dir(&sessions)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .next()
        .expect("one session dir");
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(session_dir.join("current.json")).unwrap())
            .unwrap();
    assert_eq!(json["session"]["intent"], "test drive 2");
    assert_eq!(json["session"]["scope"][0], "src/a.rs");
    let md = fs::read_to_string(session_dir.join("current.md")).unwrap();
    assert!(md.contains("## Session"));
    assert!(md.contains("- intent: test drive 2"));

    // The repo-level summary mirrors the same bytes under `run` (D3) — a
    // sessionless refresh afterwards would rewrite it without the block.
    let repo_json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(temp.path().join(".toimija/current.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(repo_json["session"]["intent"], "test drive 2");
}

#[test]
fn run_hands_the_child_a_per_session_handshake() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    // The child captures the handshake env it inherited.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--",
            "sh",
            "-c",
            "printf '%s\\n%s\\n%s\\n%s\\n%s\\n%s\\n%s\\n%s\\n%s' \"$TOIMIJA_RUN\" \"$TOIMIJA_SESSION\" \"$TOIMIJA_LIVE\" \"$TOIMIJA_LIVE_JSON\" \"$TOIMIJA_REPO_LIVE\" \"$TOIMIJA_CURRENT_MD\" \"$TOIMIJA_CURRENT_JSON\" \"$TOIMIJA_REPO_ROOT\" \"$TOIMIJA_RUN_ID\" > env.txt",
        ])
        .assert()
        .success();

    let captured = fs::read_to_string(temp.path().join("env.txt")).unwrap();
    let mut lines = captured.lines();
    assert_eq!(lines.next(), Some("1"), "TOIMIJA_RUN should be 1");
    assert!(
        !lines.next().unwrap_or_default().is_empty(),
        "TOIMIJA_SESSION should be set"
    );
    let live = lines.next().unwrap_or_default();
    assert!(
        live.contains(".toimija/sessions/") && live.ends_with("current.md"),
        "TOIMIJA_LIVE should point at the per-session packet: {live}"
    );
    let live_json = lines.next().unwrap_or_default();
    assert!(
        live_json.contains(".toimija/sessions/") && live_json.ends_with("current.json"),
        "TOIMIJA_LIVE_JSON should point at the per-session packet: {live_json}"
    );
    let repo_live = lines.next().unwrap_or_default();
    assert!(
        repo_live.ends_with(".toimija/current.md"),
        "TOIMIJA_REPO_LIVE should point at the repo-level summary: {repo_live}"
    );
    assert_eq!(lines.next(), Some(live));
    assert_eq!(lines.next(), Some(live_json));
    let repo_root = std::fs::canonicalize(lines.next().unwrap()).unwrap();
    assert_eq!(repo_root, std::fs::canonicalize(temp.path()).unwrap());
    assert_eq!(lines.next(), captured.lines().nth(1));
}

#[test]
fn run_closes_the_session_marker_when_the_child_fails_to_launch() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--", "__definitely_not_a_toimija_test_command__"])
        .assert()
        .failure();

    let sessions = temp.path().join(".toimija/sessions");
    let dirs: Vec<_> = fs::read_dir(&sessions)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect();
    assert_eq!(dirs.len(), 1, "exactly one session dir expected");
    let marker = fs::read_to_string(dirs[0].join("session.json")).unwrap();
    assert!(
        marker.contains("\"status\": \"closed\""),
        "failed launch should still auto-close the session marker: {marker}"
    );
}

#[test]
fn run_does_not_execute_the_validation_plan() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    // A validation tier that would leave a marker IF Toimija ever ran it.
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test\nvalidation:\n  fast:\n    - \"touch VALIDATION_RAN\"\nboundaries:\n  - stay in scope\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--", "true"])
        .assert()
        .success();

    assert!(
        !temp.path().join("VALIDATION_RAN").exists(),
        "run must never execute the configured validation plan"
    );
    // It still entered (refreshed the live packet).
    assert!(temp.path().join(".toimija/current.json").exists());
}
