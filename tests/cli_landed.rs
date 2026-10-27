use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

// `landed` answers "is this branch's surface already reflected in the base?" by
// content, not ancestry. These build a `trunk` base and a `work` branch, then
// simulate a squash-style landing (identical content committed onto trunk with
// no DAG link) to prove content comparison sees it even though ancestry would
// not.

mod common;
use common::{git_available, run_git};

/// Repo with a `trunk` base and a `work` branch that adds `a.txt`. Left checked
/// out on `work`.
fn setup(path: &Path) {
    run_git(path, &["init", "-q"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    run_git(path, &["checkout", "-q", "-b", "trunk"]);
    fs::write(path.join("keep.txt"), "keep\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-q", "-m", "init"]);
    run_git(path, &["checkout", "-q", "-b", "work"]);
    fs::write(path.join("a.txt"), "alpha\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-q", "-m", "add a"]);
}

/// Commit `a.txt` onto `trunk` with identical content — a squash-style landing
/// (no merge commit, no DAG link back to `work`). Leaves the repo on `work`.
fn land_a_onto_trunk(path: &Path) {
    run_git(path, &["checkout", "-q", "trunk"]);
    fs::write(path.join("a.txt"), "alpha\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-q", "-m", "land a (squash-like)"]);
    run_git(path, &["checkout", "-q", "work"]);
}

#[test]
fn landed_reports_not_landed_when_surface_is_outstanding() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    setup(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["landed", "--base", "trunk"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status: not landed"))
        .stdout(predicate::str::contains("reflected: 0/1"))
        .stdout(predicate::str::contains("a.txt"));
}

#[test]
fn landed_sees_squash_landing_that_ancestry_would_miss() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    setup(temp.path());
    land_a_onto_trunk(temp.path());

    // Ancestry would say "not landed" — work's tip is not an ancestor of trunk.
    // Content comparison sees the landing.
    let not_ancestor = std::process::Command::new("git")
        .args(["merge-base", "--is-ancestor", "work", "trunk"])
        .current_dir(temp.path())
        .status()
        .unwrap();
    assert!(
        !not_ancestor.success(),
        "precondition: work must NOT be an ancestor of trunk after a squash-like landing"
    );

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["landed", "--base", "trunk"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status: landed"));
}

#[test]
fn landed_is_ambiguous_when_base_edits_the_file_after_landing() {
    // Branch edits a.txt, trunk squash-lands that exact edit, then trunk edits
    // a.txt further. Neither forward nor reverse apply is clean, so the verdict is
    // ambiguous by default — never landed without proof. The line-containment
    // heuristic (added lines present) rides along as evidence only.
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    setup(temp.path());
    land_a_onto_trunk(temp.path());
    run_git(temp.path(), &["checkout", "-q", "trunk"]);
    fs::write(temp.path().join("a.txt"), "alpha\nbeta\n").unwrap();
    run_git(temp.path(), &["add", "."]);
    run_git(
        temp.path(),
        &["commit", "-q", "-m", "trunk edits a.txt further"],
    );
    run_git(temp.path(), &["checkout", "-q", "work"]);

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["landed", "--base", "trunk"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status: ambiguous"))
        .stdout(predicate::str::contains(
            "a.txt — added lines appear present and removed lines appear absent",
        ));
}

#[test]
fn landed_ambiguous_evidence_points_away_when_change_is_absent() {
    // The branch's change is genuinely absent from trunk, which diverged on the
    // same file differently. Still ambiguous by default (Git could not prove it),
    // but the heuristic evidence points away from landed.
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    setup(temp.path()); // work sets a.txt = "alpha\n"
    run_git(temp.path(), &["checkout", "-q", "trunk"]);
    fs::write(temp.path().join("a.txt"), "different\n").unwrap();
    run_git(temp.path(), &["add", "."]);
    run_git(temp.path(), &["commit", "-q", "-m", "trunk diverges"]);
    run_git(temp.path(), &["checkout", "-q", "work"]);

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["landed", "--base", "trunk"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status: ambiguous"))
        .stdout(predicate::str::contains(
            "a.txt — added lines are not all present in base",
        ));
}

#[test]
fn landed_reports_partial_when_some_paths_remain() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    setup(temp.path());
    // work also adds b.txt; only a.txt lands on trunk.
    fs::write(temp.path().join("b.txt"), "beta\n").unwrap();
    run_git(temp.path(), &["add", "."]);
    run_git(temp.path(), &["commit", "-q", "-m", "add b"]);
    land_a_onto_trunk(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["landed", "--base", "trunk"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status: partially landed"))
        .stdout(predicate::str::contains("reflected: 1/2"))
        .stdout(predicate::str::contains("b.txt"));
}

#[test]
fn landed_is_unknown_without_a_merge_base() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    setup(temp.path());

    // No merge-base with an unrelated/absent ref → unknown, not a crash.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["landed", "--base", "does-not-exist"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status: unknown"))
        .stdout(predicate::str::contains("no merge-base"));
}

#[test]
fn landed_json_is_valid() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    setup(temp.path());

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["landed", "--base", "trunk", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let value: serde_json::Value =
        serde_json::from_slice(&output).expect("landed --json must emit valid JSON");
    assert_eq!(value["status"], "not_landed");
    assert_eq!(value["touched"], 1);
}

#[test]
fn landed_outside_git_repo_fails() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("landed")
        .assert()
        .failure();
}

/// A branch at its base, or behind it, has nothing the base lacks: every
/// commit on HEAD is reachable from the base. That is landed, proven by
/// ancestry, not `unknown` for want of a branch diff.
#[test]
fn landed_when_head_is_the_base_or_behind_it() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    setup(temp.path());
    run_git(temp.path(), &["checkout", "-q", "trunk"]);

    let landed = |expected_reason: &str| {
        Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(temp.path())
            .args(["landed", "--base", "trunk"])
            .assert()
            .success()
            .stdout(predicate::str::contains("status: landed"))
            .stdout(predicate::str::contains(expected_reason.to_string()));
    };
    // HEAD is the base.
    landed("HEAD is in `trunk`");

    // HEAD is behind the base: the base moved on after HEAD.
    run_git(temp.path(), &["merge", "-q", "--ff-only", "work"]);
    run_git(temp.path(), &["checkout", "-q", "--detach", "trunk~1"]);
    landed("HEAD is in `trunk`");

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["landed", "--base", "trunk", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["status"], "landed");
    assert_eq!(value["touched"], 0);
}

/// A fresh branch off the base, with work not yet committed, is HEAD-in-base
/// by ancestry, but its work is in no commit, so it is not in the base: a
/// false "landed" is the dangerous direction for cleanup. Toimija's own files
/// are not the repository's work, and leave it landed.
#[test]
fn head_in_the_base_with_uncommitted_work_is_not_landed() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    setup(temp.path());
    run_git(temp.path(), &["checkout", "-q", "trunk"]);
    run_git(temp.path(), &["switch", "-q", "-c", "fresh"]);
    // Toimija's own files only, visible to Git (no ignore rules here).
    fs::create_dir_all(temp.path().join(".toimija/sessions/1-1")).unwrap();
    fs::write(temp.path().join(".toimija/current.json"), "{}\n").unwrap();
    fs::write(
        temp.path().join(".toimija/sessions/1-1/session.json"),
        "{}\n",
    )
    .unwrap();
    let report = |args: &[&str]| -> serde_json::Value {
        let output = Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(temp.path())
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    };
    let value = report(&["landed", "--base", "trunk", "--json"]);
    assert_eq!(value["status"], "landed", "{value}");

    for work in ["modified", "untracked"] {
        if work == "modified" {
            fs::write(temp.path().join("keep.txt"), "kept, then edited\n").unwrap();
        } else {
            run_git(temp.path(), &["checkout", "-q", "--", "keep.txt"]);
            fs::write(temp.path().join("new.txt"), "new\n").unwrap();
        }
        let value = report(&["landed", "--base", "trunk", "--json"]);
        assert_eq!(value["status"], "not_landed", "{work}: {value}");
        let reason = value["reason"].as_str().unwrap();
        assert!(reason.contains("uncommitted work"), "{work}: {reason}");
        Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(temp.path())
            .args(["landed", "--base", "trunk"])
            .assert()
            .success()
            .stdout(predicate::str::contains("status: not landed"));
    }
}
