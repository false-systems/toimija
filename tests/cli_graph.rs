use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

// `graph` shows the branches of a clone that are not yet in the base, each
// with its proof, its review level and the open sessions on it.

mod common;
use common::{git_available, run_git};

fn setup(path: &Path) {
    run_git(path, &["init", "-q", "-b", "main"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("keep.txt"), "keep\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-q", "-m", "init"]);
    run_git(path, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    run_git(path, &["checkout", "-q", "-b", "work"]);
    fs::write(path.join("a.txt"), "alpha\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-q", "-m", "add a"]);
}

#[test]
fn graph_lists_the_branch_and_the_base_with_no_review() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(dir.path())
        .arg("graph")
        .assert()
        .success()
        .stdout(predicate::str::contains("work"))
        .stdout(predicate::str::contains("origin/main"))
        .stdout(predicate::str::contains("review: none"));
}

#[test]
fn graph_json_names_its_schema_and_the_tips() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(dir.path())
        .args(["graph", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let graph: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(graph["schema"], "toimija-graph.v1");
    assert_eq!(graph["base"], "origin/main");
    let branches: Vec<String> = graph["lines"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|line| line["tip"]["branches"].as_array())
        .flatten()
        .map(|name| name.as_str().unwrap().to_string())
        .collect();
    assert!(branches.contains(&"work".to_string()), "{branches:?}");
}

#[test]
fn graph_shows_a_session_open_on_the_branch_with_its_intent() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let toimija = assert_cmd::cargo::cargo_bin("toimija");
    // The session is open while the command it wraps runs: the graph, here.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(dir.path())
        .args(["run", "--intent", "fix the watcher", "--"])
        .arg(&toimija)
        .arg("graph")
        .assert()
        .success()
        .stdout(predicate::str::contains("live: \"fix the watcher\""));
}

#[test]
fn graph_outside_a_repository_fails() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(dir.path())
        .arg("graph")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a Git repository"));
}
