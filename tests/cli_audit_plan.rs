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
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("src/lib.rs"), "pub fn demo() {}\n").unwrap();
    fs::create_dir_all(path.join(".toimija")).unwrap();
    fs::write(
        path.join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test fixture\ncontext:\n  safe_to_ignore:\n    - vendor/\nboundaries:\n  - stay inside fixture\n",
    )
    .unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "initial"]);
}

#[test]
fn audit_plan_renders_summary_and_surface_packet() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["audit-plan", "--surfaces", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("surface 1/2"))
        .stdout(predicate::str::contains("Request one workset"));

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["audit-plan", "--surface", "1", "--surfaces", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Toimija Audit Surface 1/2"))
        .stdout(predicate::str::contains("## Workset contract"))
        .stdout(predicate::str::contains("## Surface"))
        .stdout(predicate::str::contains("## Validation plan"));
}

#[test]
fn audit_plan_rejects_out_of_range_surface() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["audit-plan", "--surface", "2", "--surfaces", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("valid range is 1..=1"));
}

#[test]
fn audit_plan_reads_only_committed_prior_findings_and_honors_exclusions() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::create_dir_all(temp.path().join("docs/audit/2026-01-01")).unwrap();
    fs::write(
        temp.path().join("docs/audit/2026-01-01/A-findings.md"),
        "committed finding\n",
    )
    .unwrap();
    fs::create_dir_all(temp.path().join("docs/audit/2027-01-01")).unwrap();
    fs::write(
        temp.path().join("docs/audit/2027-01-01/B-findings.md"),
        "uncommitted finding\n",
    )
    .unwrap();
    fs::create_dir_all(temp.path().join("vendor")).unwrap();
    fs::write(temp.path().join("vendor/noise.rs"), "noise\n").unwrap();
    run_git(temp.path(), &["add", "docs/audit/2026-01-01"]);
    run_git(temp.path(), &["commit", "-m", "record audit finding"]);

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["audit-plan", "--surface", "1", "--budget", "4000"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "docs/audit/2026-01-01/A-findings.md",
        ))
        .stdout(predicate::str::contains("2027-01-01/B-findings.md").not())
        .stdout(predicate::str::contains("vendor").not());
}

#[test]
fn audit_surface_budget_preserves_contract_and_surface() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["audit-plan", "--surface", "1", "--budget", "100"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Output truncated"))
        .stdout(predicate::str::contains("## Workset contract"))
        .stdout(predicate::str::contains("## Surface"));
}
