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
fn graph_exits_4_and_says_what_is_missing_when_a_session_marker_is_unreadable() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    fs::create_dir_all(dir.path().join(".toimija/sessions")).unwrap();
    fs::write(
        dir.path().join(".toimija/sessions/broken.json"),
        "not a marker",
    )
    .unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(dir.path())
        .arg("graph")
        .assert()
        .code(4)
        .stdout(predicate::str::contains("work"))
        .stdout(predicate::str::contains("warning: the sessions of"));
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

/// The git subcommands `toimija graph` starts in `dir`, by Git's own trace.
fn git_verbs(dir: &Path) -> Vec<String> {
    let trace = dir.join(".git/graph-trace.json");
    let _ = fs::remove_file(&trace);
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(dir)
        .env("GIT_TRACE2_EVENT", &trace)
        .args(["-q", "graph"])
        .assert()
        .success();
    fs::read_to_string(&trace)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event"] == "start")
        .filter_map(|event| {
            event["argv"].as_array().unwrap()[1..]
                .iter()
                .filter_map(|arg| arg.as_str())
                .find(|arg| !arg.starts_with('-'))
                .map(str::to_string)
        })
        .collect()
}

/// Holding a review against its base costs one `git merge-base` for each tip
/// that has a review, and nothing for a tip that has none.
#[test]
fn the_base_check_costs_one_git_process_per_reviewed_tip_and_none_otherwise() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    // A second branch in progress, so there are two tips beside the base.
    run_git(dir.path(), &["checkout", "-q", "-b", "other", "main"]);
    fs::write(dir.path().join("b.txt"), "beta\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-q", "-m", "add b"]);

    let merge_bases = |verbs: &[String]| verbs.iter().filter(|verb| *verb == "merge-base").count();
    let unreviewed = git_verbs(dir.path());
    assert!(
        unreviewed.iter().any(|verb| verb == "log"),
        "{unreviewed:?}"
    );
    assert_eq!(merge_bases(&unreviewed), 0, "{unreviewed:?}");

    // One of the two tips gets a review, read against the base.
    let rev = |spec: &str| {
        let output = common::git_at(dir.path())
            .args(["rev-parse", spec])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    let note = format!(
        r#"{{"schema":"sykli-review.v1","level":"fine","base":"{}","overruled":null}}"#,
        rev("origin/main")
    );
    run_git(
        dir.path(),
        &[
            "notes",
            "--ref",
            "refs/notes/sykli/review",
            "add",
            "-m",
            &note,
            &rev("work^{tree}"),
        ],
    );
    let reviewed = git_verbs(dir.path());
    assert_eq!(merge_bases(&reviewed), 1, "{reviewed:?}");
    assert_eq!(
        reviewed.len(),
        unreviewed.len() + 3,
        "a reviewed tip adds the notes list, its note and its merge base: {reviewed:?}"
    );

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(dir.path())
        .arg("graph")
        .assert()
        .success()
        .stdout(predicate::str::contains("review: fine"));
    // The base moves on and the branch takes it in. A review of the new tree
    // that was read against the earlier base is not a review of it as it
    // stands.
    run_git(dir.path(), &["checkout", "-q", "work"]);
    run_git(dir.path(), &["merge", "-q", "--no-edit", "other"]);
    run_git(
        dir.path(),
        &["update-ref", "refs/remotes/origin/main", "other"],
    );
    run_git(
        dir.path(),
        &[
            "notes",
            "--ref",
            "refs/notes/sykli/review",
            "add",
            "-m",
            &note,
            &rev("work^{tree}"),
        ],
    );
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(dir.path())
        .arg("graph")
        .assert()
        .success()
        .stdout(predicate::str::contains("review: stale"))
        .stdout(predicate::str::contains("review: fine").not());
}
