use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

mod common;
use common::{git_available, run_git};

fn init_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("README.md"), "# Demo\n").unwrap();
    run_git(path, &["add", "README.md"]);
    run_git(path, &["commit", "-m", "initial"]);
}

#[test]
fn validate_without_plan_is_rejected() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("validate")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "`toimija validate` only supports `--plan`",
        ));
}

#[test]
fn validate_plan_uses_repo_config_when_present() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: demo\nvalidation:\n  fast:\n    - just test-fast\n  strict:\n    - just test-strict\n  full:\n    - just test-full\n  privileged:\n    - just test-privileged\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["validate", "--plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("source: repo config"))
        .stdout(predicate::str::contains("just test-fast"))
        .stdout(predicate::str::contains("just test-strict"))
        .stdout(predicate::str::contains("just test-full"))
        .stdout(predicate::str::contains("just test-privileged"))
        .stdout(predicate::str::contains("cargo test").not())
        .stdout(predicate::str::contains("mix test").not());
}

#[test]
fn validate_plan_adds_repo_checks_to_inferred_rust_checks() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "validation:\n  privileged:\n    - just integration\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["validate", "--plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("source: inferred and repo config"))
        .stdout(predicate::str::contains("cargo test --workspace"))
        .stdout(predicate::str::contains("cargo fmt --check"))
        .stdout(predicate::str::contains("cargo clippy"))
        .stdout(predicate::str::contains("just integration"));
}

#[test]
fn validate_plan_replaces_inference_only_when_explicit() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "validation:\n  replace_inferred: true\n  fast:\n    - just test\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["validate", "--plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("source: repo config"))
        .stdout(predicate::str::contains("just test"))
        .stdout(predicate::str::contains("explicitly replaces inferred"))
        .stdout(predicate::str::contains("cargo test").not())
        .stdout(predicate::str::contains("cargo fmt").not())
        .stdout(predicate::str::contains("cargo clippy").not());
}

#[test]
fn validate_plan_infers_rust_commands() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["validate", "--plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("source: inferred"))
        .stdout(predicate::str::contains("cargo test --workspace"))
        .stdout(predicate::str::contains("cargo fmt --check"))
        .stdout(predicate::str::contains(
            "Validation inferred from Rust repo shape, not proven by Sykli",
        ));
}

#[test]
fn validate_plan_for_generic_repo_is_unknown() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["validate", "--plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("source: unknown"))
        .stdout(predicate::str::contains(
            "validation unavailable for generic repo without repo config",
        ));
}

#[test]
fn validate_plan_outside_git_repo_fails() {
    let temp = tempfile::tempdir().unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["validate", "--plan"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a Git repository"));
}

const SYKLI_GATE: &str = "mkdir -p .sykli && sykli run sykli.json --json > .sykli/receipt.json && sykli verify .sykli/receipt.json --contract sykli.json";

fn write_sykli_contract(path: &Path) {
    fs::write(
        path.join("sykli.json"),
        r#"{
  "schema": "sykli-contract.v1",
  "tasks": [
    { "name": "fmt", "run": "cargo fmt --check", "inputs": ["Cargo.toml"] },
    { "name": "test", "run": "cargo test", "inputs": ["Cargo.toml"] }
  ]
}
"#,
    )
    .unwrap();
}

#[test]
fn validate_plan_names_the_sykli_gate_when_the_repo_declares_one() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    write_sykli_contract(temp.path());

    // The repo's declared gate is the plan — not commands guessed from the
    // repo's shape — and nothing says it is "not proven by Sykli".
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["validate", "--plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("source: sykli.json"))
        .stdout(predicate::str::contains(SYKLI_GATE))
        .stdout(predicate::str::contains("tasks: fmt, test"))
        .stdout(predicate::str::contains("not proven by Sykli").not())
        .stdout(predicate::str::contains("cargo test --workspace").not());
}

#[test]
fn context_packet_carries_the_sykli_gate_without_an_inferred_warning() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(temp.path().join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    write_sykli_contract(temp.path());

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["context", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let packet: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let plan = &packet["validation_plan"];
    assert_eq!(plan["source"], "sykli");
    assert_eq!(plan["gate"]["contract"], "sykli.json");
    assert_eq!(plan["gate"]["command"], SYKLI_GATE);
    assert_eq!(plan["gate"]["tasks"], serde_json::json!(["fmt", "test"]));
    // A declared gate replaces the guessed tiers, the focused tier included.
    assert_eq!(plan["focused"], serde_json::json!([]));
    assert_eq!(plan["fast"], serde_json::json!([]));
    let warnings = packet["warnings"].to_string();
    assert!(!warnings.contains("inferred"), "{warnings}");

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("### Gate (sykli.json)"))
        .stdout(predicate::str::contains(SYKLI_GATE))
        .stdout(predicate::str::contains("### Fast").not());
}
