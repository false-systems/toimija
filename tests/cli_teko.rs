use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

mod common;
use common::{git_available, run_git};

fn init(path: &Path) {
    run_git(path, &["init", "-b", "main"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("README.md"), "base\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "init"]);
}

fn new_spec(path: &Path, id: &str, scope_gate: &str) {
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(path)
        .args([
            "teko",
            "new",
            "--id",
            id,
            "--title",
            "Fix final-only action loss",
            "--objective",
            "Make completion truthful",
            "--goal",
            "regression=regression test exists",
            "--gate",
            &format!("test={scope_gate}"),
            "--completion",
            "validated",
            "--ratify",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("created spec"));
}

fn start(path: &Path, spec: &str, work: &str, scope: &str) {
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(path)
        .args([
            "teko", "start", "--spec", spec, "--id", work, "--scope", scope,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("active work"));
}

#[test]
fn complete_workflow_persists_invalidates_and_refuses_false_closure() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    new_spec(temp.path(), "spec", "test -f changed.txt");
    start(temp.path(), "spec", "work", "changed.txt");

    // Restart boundaries are real: every assertion below is a new process.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1/4 complete"));

    fs::write(temp.path().join("changed.txt"), "one\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "close"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Cannot close"))
        .stderr(predicate::str::contains("regression test exists"))
        .stderr(predicate::str::contains("test -f changed.txt"));

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "verify", "--claim", "regression"])
        .assert()
        .success()
        .stdout(predicate::str::contains("4/4 complete"))
        .stdout(predicate::str::contains("passed on current tree"));

    // A new tree invalidates, but does not erase, the old receipt.
    fs::write(temp.path().join("changed.txt"), "two\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("stale — passed on previous tree"));
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "close"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("passed on tree"))
        .stderr(predicate::str::contains("current tree is"));

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "verify"])
        .assert()
        .success()
        .stdout(predicate::str::contains("4/4 complete"));
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "close", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"work_id\": \"work\""));
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No active Teko"));
}

#[test]
fn one_active_work_and_scope_are_enforced() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    new_spec(temp.path(), "one", "true");
    new_spec(temp.path(), "two", "true");
    start(temp.path(), "one", "work-one", "src");

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "start", "--spec", "two", "--id", "work-two"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already active"));

    fs::write(temp.path().join("README.md"), "outside\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "show", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"scope-violated\""))
        .stdout(predicate::str::contains("README.md"));
}

#[test]
fn drafts_require_explicit_ratification_and_empty_store_is_safe() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "show", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"active\":null"));

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "teko",
            "new",
            "--id",
            "draft",
            "--title",
            "Draft",
            "--objective",
            "wait",
            "--goal",
            "goal",
            "--completion",
            "changed",
        ])
        .assert()
        .success();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "start", "--spec", "draft"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("explicit ratification"));
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "start", "--spec", "draft", "--ratify"])
        .assert()
        .success();
}
