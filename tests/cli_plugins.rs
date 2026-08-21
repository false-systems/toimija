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

/// The packet renders providers role-first (`work: teko`), so `inspect work` is
/// the natural thing to type. The refusal has to say what the argument is
/// rather than report a missing kind, which reads as a configuration fault in
/// the repo rather than a wrong argument.
#[test]
fn inspecting_a_role_instead_of_a_plugin_says_so() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["plugins", "inspect", "work"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown plugin `work`"))
        .stderr(predicate::str::contains("did you mean `teko`"))
        .stderr(predicate::str::contains("plugins doctor"));
}

#[test]
fn an_unknown_plugin_names_the_way_to_list_them() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["plugins", "inspect", "nonsense"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown plugin `nonsense`"))
        // No role hint for something that is not a role.
        .stderr(predicate::str::contains("did you mean").not())
        .stderr(predicate::str::contains("plugins doctor"));
}
