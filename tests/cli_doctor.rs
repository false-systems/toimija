use assert_cmd::Command;
use predicates::prelude::*;
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

fn init_git_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("README.md"), "# Demo\n").unwrap();
    run_git(path, &["add", "README.md"]);
    run_git(path, &["commit", "-m", "initial"]);
}

fn write_ready_rust_repo(path: &Path) {
    fs::write(
        path.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(path.join("AGENTS.md"), "# Agent guide\n").unwrap();
    fs::write(path.join("CLAUDE.md"), "See AGENTS.md.\n").unwrap();
    fs::create_dir_all(path.join(".toimija")).unwrap();
    fs::write(
        path.join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test fixture\ncontext:\n  important_docs:\n    - README.md\nvalidation:\n  fast:\n    - cargo test --workspace\nboundaries:\n  - stay inside fixture\n",
    )
    .unwrap();
}

/// A repo whose agent docs honour the full contract: they route through the
/// `toimija context` preflight and mirror every configured boundary. This is
/// what `--strict` requires, so it is its own fixture rather than a tweak of
/// `write_ready_rust_repo` (whose docs are intentionally minimal).
fn write_strict_ready_rust_repo(path: &Path) {
    fs::write(
        path.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        path.join("AGENTS.md"),
        "# Agent guide\n\nBefore editing, run `toimija context --for-diff`.\n\nStay inside fixture.\n",
    )
    .unwrap();
    fs::write(path.join("CLAUDE.md"), "See AGENTS.md.\n").unwrap();
    fs::create_dir_all(path.join(".toimija")).unwrap();
    fs::write(
        path.join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test fixture\ncontext:\n  important_docs:\n    - README.md\nvalidation:\n  fast:\n    - cargo test --workspace\nboundaries:\n  - Stay inside fixture.\n",
    )
    .unwrap();
}

#[test]
fn doctor_outside_git_repo_reports_failure() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains("fail: inside git repo"));
}

#[test]
fn doctor_ready_rust_repo_succeeds() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_ready_rust_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: toimija config - config found",
        ))
        .stdout(predicate::str::contains(
            "ok: repo role - repo role present",
        ))
        .stdout(predicate::str::contains(
            "ok: repo kind - Rust repo detected",
        ))
        .stdout(predicate::str::contains(
            "ok: validation plan - validation configured",
        ))
        .stdout(predicate::str::contains(
            "ok: CLAUDE.md - CLAUDE.md references AGENTS.md",
        ));
}

#[test]
fn doctor_missing_config_is_advisory() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::write(temp.path().join("AGENTS.md"), "# Agent guide\n").unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "warn: toimija config - missing config; run `toimija init`",
        ))
        .stdout(predicate::str::contains(
            "warn: repo role - repo role is empty",
        ))
        .stdout(predicate::str::contains(
            "warn: boundaries - boundaries are empty",
        ));
}

#[test]
fn doctor_invalid_config_fails() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(temp.path().join(".toimija/repo.yaml"), "repo: [").unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "fail: toimija config - invalid repo config",
        ));
}

#[test]
fn doctor_reports_drift_as_advisory_by_default() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    // Minimal docs: no preflight mention, boundary not mirrored. Plain doctor
    // surfaces these as warnings but still exits 0.
    write_ready_rust_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "warn: preflight mention - no agent doc mentions the `toimija enter` preflight",
        ))
        .stdout(predicate::str::contains("warn: boundaries mirrored -"));
}

#[test]
fn doctor_strict_passes_on_this_repo() {
    // Dogfood: the README tells people to run `toimija doctor --strict` as the
    // PR/CI gate, so this repo must pass its own gate. Guards against boundary
    // wording in .toimija/repo.yaml drifting from AGENTS.md/CLAUDE.md.
    if !git_available() {
        return;
    }
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["doctor", "--strict"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: boundaries mirrored - boundaries appear in the agent docs",
        ));
}

#[test]
fn doctor_strict_ready_repo_succeeds() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_strict_ready_rust_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["doctor", "--strict"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Toimija doctor (strict)"))
        .stdout(predicate::str::contains(
            "ok: preflight mention - agent docs point at the `toimija enter` preflight",
        ))
        .stdout(predicate::str::contains(
            "ok: boundaries mirrored - boundaries appear in the agent docs",
        ));
}

#[test]
fn doctor_strict_escalates_drift_to_failure() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    // Same minimal-docs repo that only warns under plain doctor.
    write_ready_rust_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["doctor", "--strict"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "fail: preflight mention - no agent doc mentions the `toimija enter` preflight",
        ))
        .stdout(predicate::str::contains("fail: boundaries mirrored -"));
}

#[test]
fn doctor_strict_flags_missing_important_doc() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_strict_ready_rust_repo(temp.path());
    // Nominate a doc that does not exist on disk.
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test fixture\ncontext:\n  important_docs:\n    - README.md\n    - docs/MISSING.md\nboundaries:\n  - Stay inside fixture.\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["doctor", "--strict"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "fail: important docs - missing on disk: docs/MISSING.md",
        ));
}

#[test]
fn doctor_claude_without_agents_reference_is_advisory() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_ready_rust_repo(temp.path());
    fs::write(temp.path().join("CLAUDE.md"), "Local Claude notes.\n").unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "warn: CLAUDE.md - CLAUDE.md exists but does not reference AGENTS.md",
        ));
}
