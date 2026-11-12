//! Public-interface P0 tests: state preservation, an interrupted Git
//! operation shown, and structured-output hygiene.

use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;

mod common;
use common::{fixture::RepoFixture, git_available};

fn toimija(path: &Path) -> Command {
    let mut command = Command::cargo_bin("toimija").unwrap();
    command.current_dir(path);
    command
}

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).unwrap()
}

#[test]
fn public_json_preserves_staged_unstaged_and_untracked_intent() {
    if !git_available() {
        return;
    }
    let repo = RepoFixture::new();
    repo.write("tracked.txt", "base\n");
    repo.commit("initial");

    repo.write("tracked.txt", "staged\n");
    repo.stage(&["tracked.txt"]);
    repo.write("tracked.txt", "unstaged\n");
    repo.write("scratch.txt", "untracked\n");

    let value = json_output(
        toimija(repo.path())
            .args(["enter", "--json"])
            .assert()
            .success(),
    );
    let staged = value["git"]["staged_files"].as_array().unwrap();
    let unstaged = value["git"]["unstaged_files"].as_array().unwrap();
    let untracked = value["git"]["untracked_files"].as_array().unwrap();
    assert!(staged.iter().any(|file| file["path"] == "tracked.txt"));
    assert!(unstaged.iter().any(|file| file["path"] == "tracked.txt"));
    assert!(untracked.iter().any(|file| file == "scratch.txt"));
}

#[test]
fn an_interrupted_merge_is_visible_in_the_packet() {
    if !git_available() {
        return;
    }
    let repo = RepoFixture::new();
    repo.write("conflict.txt", "base\n");
    repo.commit("initial");
    repo.branch("feature");
    repo.write("conflict.txt", "feature\n");
    repo.stage(&["conflict.txt"]);
    repo.commit("feature change");
    repo.switch("main");
    repo.write("conflict.txt", "main\n");
    repo.stage(&["conflict.txt"]);
    repo.commit("main change");
    repo.git_expect_failure(&["merge", "feature"]);

    let value = json_output(
        toimija(repo.path())
            .args(["enter", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(value["git_operation"]["kind"], "merge");
    assert!(
        value["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("Conflicted"))
    );

    let markdown = toimija(repo.path()).arg("enter").assert().success();
    let markdown = String::from_utf8_lossy(&markdown.get_output().stdout).into_owned();
    assert!(
        markdown.contains("- operation: merge in progress\n"),
        "{markdown}"
    );
}

#[test]
fn json_stdout_stays_parseable_when_diagnostics_are_enabled() {
    if !git_available() {
        return;
    }
    let repo = RepoFixture::new();
    repo.write("README.md", "# QA\n");
    repo.commit("initial");
    let output = toimija(repo.path())
        .args(["-v", "enter", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    serde_json::from_slice::<Value>(&output.stdout).unwrap();
    assert!(!output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("toimija.context.rendered"));
}

#[test]
fn identical_context_json_has_stable_semantic_ordering() {
    if !git_available() {
        return;
    }
    let repo = RepoFixture::new();
    repo.write("README.md", "# QA\n");
    repo.commit("initial");
    let first = json_output(
        toimija(repo.path())
            .args(["enter", "--json"])
            .assert()
            .success(),
    );
    let second = json_output(
        toimija(repo.path())
            .args(["enter", "--json"])
            .assert()
            .success(),
    );
    assert_eq!(first["git"], second["git"]);
    assert_eq!(first["validation_plan"], second["validation_plan"]);
    assert_eq!(first["read_first"], second["read_first"]);
    assert_eq!(first["git_operation"], second["git_operation"]);
}
