use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

mod common;
use common::{git_available, run_git};

fn init_git_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
}

fn record(id: &str, recorded_at: u64) -> String {
    format!(
        r#"{{"version":1,"id":"{id}","agent":["codex"],"intent":"test","scope":[],"branch":"main","started_at":1,"last_seen_at":2,"end":"closed","exit_status":0,"handoffs":[],"handoffs_total":0,"recorded_at":{recorded_at}}}"#
    )
}

#[test]
fn history_lists_newest_first_and_json_emits_all_records() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/history.jsonl"),
        format!("{}\n{}\n", record("100-1", 100), record("200-2", 200)),
    )
    .unwrap();

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("history")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.find("200-2").unwrap() < text.find("100-1").unwrap());

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["history", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let records: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(records.as_array().unwrap().len(), 2);
}

#[test]
fn history_skips_malformed_lines_with_a_counted_warning() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/history.jsonl"),
        format!("{}\n{{broken\n", record("100-1", 100)),
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("history")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "1 malformed history line(s) skipped",
        ))
        .stderr(predicate::str::contains("malformed"));
}

#[test]
fn run_teardown_records_a_closed_session_without_waiting_for_prune() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--intent", "history test", "--", "true"])
        .assert()
        .success();

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["history", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let records: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(records.as_array().unwrap().len(), 1);
    assert_eq!(records[0]["end"], "closed");
    assert_eq!(records[0]["intent"], "history test");
    assert_eq!(records[0]["exit_status"], 0);
}
