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
        .stdout(predicate::str::contains(
            "Enter, watch while an agent command runs",
        ))
        .stdout(predicate::str::contains(
            "Append or inspect a session handoff record",
        ))
        .stdout(predicate::str::contains("Print suggested preflight text"))
        .stdout(predicate::str::contains(
            "Check whether this branch's work has landed",
        ))
        .stdout(predicate::str::contains("Print the validation plan"));
}

#[test]
fn top_level_help_shows_the_agent_workflow_and_boundaries() {
    Command::cargo_bin("toimija")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        // The typical agent workflow, with a copy-pasteable entry example.
        .stdout(predicate::str::contains("Typical agent workflow"))
        .stdout(predicate::str::contains(
            "toimija enter --for-diff --budget 4000",
        ))
        // The boundary framing that makes Toimija safe to trust.
        .stdout(predicate::str::contains("advisory and local-only"))
        .stdout(predicate::str::contains("never touches"));
}

#[test]
fn every_subcommand_help_says_when_to_use_it() {
    // Each subcommand's long help must carry a "use this …" orientation line so
    // an agent can pick between siblings (enter vs context vs refresh, …).
    for subcommand in [
        "init",
        "doctor",
        "context",
        "current",
        "refresh",
        "enter",
        "watch",
        "run",
        "intent",
        "handoff",
        "snippet",
        "landed",
        "sessions",
        "validate",
        "verify",
        "snapshot",
        "audit-plan",
    ] {
        Command::cargo_bin("toimija")
            .unwrap()
            .args([subcommand, "--help"])
            .assert()
            .success()
            .stdout(
                predicate::str::contains("Use this")
                    .or(predicate::str::contains("use this"))
                    .name(subcommand),
            );
    }
}

#[test]
fn high_traffic_subcommands_carry_examples() {
    for (subcommand, example) in [
        ("enter", "toimija enter --for-diff --budget 4000"),
        ("run", "toimija run --intent"),
        ("sessions", "toimija sessions --prune"),
        ("intent", "toimija intent"),
        ("handoff", "toimija handoff --show"),
        ("current", "toimija current --verify --json"),
        ("verify", "toimija verify --json"),
        ("landed", "toimija landed"),
        ("doctor", "toimija doctor --strict"),
        ("snapshot", "toimija snapshot --lease"),
        ("context", "toimija context --for-diff"),
        ("audit-plan", "toimija audit-plan --surface"),
    ] {
        Command::cargo_bin("toimija")
            .unwrap()
            .args([subcommand, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Examples:").name(subcommand))
            .stdout(predicate::str::contains(example).name(subcommand));
    }
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
