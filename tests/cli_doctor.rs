use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn doctor_outside_git_repo_reports_failure() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains("fail: inside git repo"));
}
