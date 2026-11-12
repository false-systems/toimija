use assert_cmd::Command;
use std::fs;
use std::path::Path;

mod common;
use common::{git_available, run_git};

fn init_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("README.md"), "# Demo\n").unwrap();
    fs::write(
        path.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::create_dir_all(path.join(".toimija")).unwrap();
    fs::write(
        path.join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test fixture\nboundaries:\n  - stay inside fixture\n",
    )
    .unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "initial"]);
}

#[test]
fn verbose_json_logs_go_to_stderr_and_keep_stdout_parseable() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["-v", "--log-format", "json", "enter", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    let context: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(context["repo"]["name"].as_str(), Some("demo"));

    let events = log_events(&stderr);
    assert!(events.iter().any(|event| event == "toimija.command.start"));
    assert!(events.iter().any(|event| event == "toimija.config.loaded"));
    assert!(
        events
            .iter()
            .any(|event| event == "toimija.context.rendered")
    );
    assert!(events.iter().any(|event| event == "toimija.command.finish"));
}

#[test]
fn run_emits_the_command_finish_bookend_with_the_wrapped_exit_code() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());

    // `run` adopts the wrapped command's exit code, which must not bypass
    // the finish event via `process::exit`. Both must hold at once.
    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "-v",
            "--log-format",
            "json",
            "run",
            "--intent",
            "test",
            "--",
            "sh",
            "-c",
            "exit 7",
        ])
        .assert()
        .code(7);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    let finish = log_lines(&stderr)
        .into_iter()
        .find(|line| line["fields"]["event"] == "toimija.command.finish")
        .expect("run should emit toimija.command.finish");
    assert_eq!(finish["fields"]["exit_code"].as_i64(), Some(7));
}

#[test]
fn quiet_mode_suppresses_logs_without_suppressing_stdout() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["--quiet", "--log-format", "json", "enter", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    let context: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(context["repo"]["name"].as_str(), Some("demo"));
    assert_eq!(stderr, "");
}

/// A failure is said once, in words for a person. Its finish event sits at
/// info beside the start event, so `-v` shows both; by default it does not
/// repeat the error as a log line first.
#[test]
fn a_failure_is_said_once_and_its_finish_event_waits_for_verbose() {
    let temp = tempfile::tempdir().unwrap();
    let toimija = || {
        let mut command = Command::cargo_bin("toimija").unwrap();
        command
            .current_dir(temp.path())
            .env("GIT_CEILING_DIRECTORIES", temp.path().parent().unwrap())
            .env_remove("RUST_LOG");
        command
    };

    let said = toimija().arg("enter").assert().failure();
    let stderr = String::from_utf8(said.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("not a Git repository"), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "said more than once: {stderr}");

    let verbose = toimija()
        .args(["-v", "--log-format", "json", "enter"])
        .assert()
        .failure();
    let stderr = String::from_utf8(verbose.get_output().stderr.clone()).unwrap();
    let finish = log_lines(&stderr)
        .into_iter()
        .find(|line| line["fields"]["event"] == "toimija.command.finish")
        .expect("a failed command should still emit toimija.command.finish under -v");
    assert_eq!(finish["level"], "INFO", "{finish}");
    assert!(
        finish["fields"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("not a Git repository")),
        "{finish}"
    );
}

fn log_events(stderr: &str) -> Vec<String> {
    log_lines(stderr)
        .into_iter()
        .map(|value| value["fields"]["event"].as_str().unwrap().to_string())
        .collect()
}

fn log_lines(stderr: &str) -> Vec<serde_json::Value> {
    stderr
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect()
}
