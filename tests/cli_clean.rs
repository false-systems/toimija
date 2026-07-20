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
