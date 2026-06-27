use assert_cmd::Command;
use predicates::prelude::*;

// `snippet` prints copy-pasteable text Toimija suggests but does not own. It
// writes no files and needs no git repo.

#[test]
fn snippet_preflight_routes_through_enter_and_validate() {
    Command::cargo_bin("toimija")
        .unwrap()
        .args(["snippet", "preflight"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "toimija enter --for-diff --budget 4000",
        ))
        .stdout(predicate::str::contains("toimija validate --plan"))
        .stdout(predicate::str::contains("Do not broad-crawl"));
}

#[test]
fn snippet_script_is_a_bash_stub() {
    Command::cargo_bin("toimija")
        .unwrap()
        .args(["snippet", "script"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("#!/usr/bin/env bash"))
        .stdout(predicate::str::contains("set -euo pipefail"))
        .stdout(predicate::str::contains("toimija enter"));
}

#[test]
fn snippet_requires_a_known_kind() {
    Command::cargo_bin("toimija")
        .unwrap()
        .args(["snippet", "nonsense"])
        .assert()
        .failure();
}
