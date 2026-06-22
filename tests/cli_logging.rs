use assert_cmd::Command;
use std::fs;
use std::path::Path;

fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_git(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

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
        .args(["-v", "--log-format", "json", "context", "--json"])
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
fn quiet_mode_suppresses_logs_without_suppressing_stdout() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["--quiet", "--log-format", "json", "context", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    let context: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(context["repo"]["name"].as_str(), Some("demo"));
    assert_eq!(stderr, "");
}

fn log_events(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .map(|value| value["fields"]["event"].as_str().unwrap().to_string())
        .collect()
}
