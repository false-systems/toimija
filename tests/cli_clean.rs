use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

mod common;
use common::{git_available, run_git};

fn set_age(path: &Path, age: Duration) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(SystemTime::now() - age))
        .unwrap();
}

#[test]
fn clean_dry_run_lists_without_deleting_then_removes_exact_expired_files() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    run_git(temp.path(), &["init"]);
    let toimija = temp.path().join(".toimija");
    let runs = toimija.join("runs");
    fs::create_dir_all(&runs).unwrap();
    fs::write(
        toimija.join("repo.yaml"),
        "stewardship:\n  runs_retention_days: 1\n",
    )
    .unwrap();
    let old_run = runs.join("old.jsonl");
    let fresh_run = runs.join("fresh.jsonl");
    let old_temp = toimija.join("current.json.tmp-123-4");
    let false_temp = toimija.join("current.json.tmp-bad-4");
    for path in [&old_run, &fresh_run, &old_temp, &false_temp] {
        fs::write(path, b"x").unwrap();
    }
    set_age(&old_run, Duration::from_secs(2 * 24 * 60 * 60));
    set_age(&old_temp, Duration::from_secs(2 * 60 * 60));
    set_age(&false_temp, Duration::from_secs(2 * 60 * 60));

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["clean", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "would remove 0 sessions, 1 transcripts, 1 temp files",
        ))
        .stdout(predicate::str::contains(".toimija/runs/old.jsonl"))
        .stdout(predicate::str::contains(".toimija/current.json.tmp-123-4"));
    assert!(old_run.exists());
    assert!(old_temp.exists());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("clean")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "removed 0 sessions, 1 transcripts, 1 temp files",
        ));
    assert!(!old_run.exists());
    assert!(!old_temp.exists());
    assert!(fresh_run.exists());
    assert!(false_temp.exists());
}

#[test]
fn clean_journals_expired_sessions_and_honors_the_history_cap() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    run_git(temp.path(), &["init"]);
    let toimija = temp.path().join(".toimija");
    let session = toimija.join("sessions/old-closed");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        toimija.join("repo.yaml"),
        "stewardship:\n  sessions_retention_days: 1\n  history_max_records: 2\n",
    )
    .unwrap();
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(
        session.join("session.json"),
        format!(
            r#"{{
                "id": "old-closed", "pid": 1, "agent": ["codex"],
                "worktree": "/repo", "branch": "main", "head": null,
                "intent": "ship", "scope": [], "started_at": {started},
                "status": "closed", "closed_at": {closed}, "exit_status": 0
            }}"#,
            started = now - 3 * 24 * 60 * 60,
            closed = now - 2 * 24 * 60 * 60,
        ),
    )
    .unwrap();
    let old_record = |id: &str| {
        format!(
            r#"{{"version":1,"id":"{id}","agent":[],"intent":null,"scope":[],"branch":null,"started_at":1,"last_seen_at":null,"end":"closed","exit_status":0,"handoffs":[],"handoffs_total":0,"recorded_at":1}}"#
        )
    };
    fs::write(
        toimija.join("history.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            old_record("prior-0"),
            old_record("prior-1"),
            old_record("prior-2")
        ),
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("clean")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "removed 1 sessions, 0 transcripts, 0 temp files; journaled 1 records",
        ));

    assert!(!session.exists());
    let records: Vec<serde_json::Value> = fs::read_to_string(toimija.join("history.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["id"], "prior-2");
    assert_eq!(records[1]["id"], "old-closed");
}
