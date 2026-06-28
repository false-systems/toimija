use assert_cmd::Command;
use predicates::prelude::*;

// `--help` exits 0 and prints to stdout; these lock in that every Toimija
// subcommand and flag carries a description, so empty help cannot silently
// return.

#[test]
fn top_level_help_describes_every_subcommand() {
    Command::cargo_bin("toimija")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Create .toimija/repo.yaml"))
        .stdout(predicate::str::contains("Check Toimija setup"))
        .stdout(predicate::str::contains(
            "Compile the current Git work state",
        ))
        .stdout(predicate::str::contains("Recompute the live packet"))
        .stdout(predicate::str::contains("Agent entry"))
        .stdout(predicate::str::contains("Watch the repo"))
        .stdout(predicate::str::contains("Print suggested preflight text"))
        .stdout(predicate::str::contains(
            "Check whether this branch's work has landed",
        ))
        .stdout(predicate::str::contains("Print the validation plan"));
}

#[test]
fn context_help_describes_every_flag() {
    Command::cargo_bin("toimija")
        .unwrap()
        .args(["context", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Emit the packet as JSON"))
        .stdout(predicate::str::contains("Add a diff-focused section"))
        .stdout(predicate::str::contains("Approximate output cap"));
}
