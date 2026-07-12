use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use std::process::Command as ProcessCommand;

mod common;
use common::{git_available, run_git};

fn init_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(
        path.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(path.join("README.md"), "# Demo\n").unwrap();
    fs::create_dir_all(path.join(".toimija")).unwrap();
    fs::write(
        path.join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: fixture\nboundaries:\n  - stay inside fixture\n",
    )
    .unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "init"]);
}

#[test]
fn adopt_creates_agents_and_second_run_is_a_noop() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    let command = || {
        Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(temp.path())
            .args(["adopt"])
            .assert()
            .success()
    };
    command().stdout(predicate::str::contains("created AGENTS.md"));
    let first = fs::read(temp.path().join("AGENTS.md")).unwrap();
    command();
    assert_eq!(fs::read(temp.path().join("AGENTS.md")).unwrap(), first);
    let first = String::from_utf8(first).unwrap();
    assert_eq!(first.matches("toimija:adopt:v1:begin").count(), 1);
}

#[test]
fn adopt_updates_existing_entry_files_and_doctor_accepts_them() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(
        temp.path().join("AGENTS.md"),
        "# Agent guide\n\ntoimija enter\nKeep v0.0 as one Rust crate.\nStay inside fixture.\n",
    )
    .unwrap();
    fs::write(temp.path().join("CLAUDE.md"), "@AGENTS.md\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["doctor", "--strict"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("doctor --strict"));
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("adopt")
        .assert()
        .success();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["doctor", "--strict"])
        .assert()
        .success()
        .stdout(predicate::str::contains("adoption"));
    assert!(temp.path().join("AGENTS.md").exists());
}

#[test]
fn adopt_refuses_to_clobber_an_unreadable_entry_file() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    // Non-UTF-8 bytes make the read fail; adopt must abort with the cause and
    // leave the file byte-identical, never overwrite it with just the block.
    let original: &[u8] = b"# Guide\n\xff\xfekeep these bytes\n";
    fs::write(temp.path().join("AGENTS.md"), original).unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("adopt")
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to read"));
    assert_eq!(fs::read(temp.path().join("AGENTS.md")).unwrap(), original);
}

#[test]
fn adopt_hooks_install_refresh_hooks_and_refuse_foreign_hooks() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["adopt", "--hooks"])
        .assert()
        .success()
        .stdout(predicate::str::contains("For you to place"))
        .stdout(predicate::str::contains("SessionStart"))
        .stdout(predicate::str::contains(
            "shared by every worktree of this clone",
        ));
    let hooks = temp.path().join(".git/hooks");
    for name in ["post-checkout", "post-merge", "post-commit"] {
        let text = fs::read_to_string(hooks.join(name)).unwrap();
        assert!(text.contains("toimija:adopt-hooks:v1"));
        assert!(text.contains("unset GIT_DIR"));
        assert!(text.contains("|| true"));
    }
    fs::write(hooks.join("post-commit"), "#!/bin/sh\necho foreign\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["adopt", "--hooks"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to replace foreign Git hook",
        ));
    assert_eq!(
        fs::read_to_string(hooks.join("post-commit")).unwrap(),
        "#!/bin/sh\necho foreign\n"
    );
}

#[cfg(unix)]
#[test]
fn hook_scrubs_git_location_environment_before_refresh() {
    if !git_available() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    init_repo(other.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["adopt", "--hooks"])
        .assert()
        .success();

    let bin = temp.path().join("fake-bin");
    fs::create_dir(&bin).unwrap();
    let fake = bin.join("toimija");
    fs::write(
        &fake,
        "#!/bin/sh\ngit rev-parse --show-toplevel > hook-root\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake, permissions).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let status = ProcessCommand::new("sh")
        .current_dir(temp.path())
        .arg(temp.path().join(".git/hooks/post-checkout"))
        .env("PATH", path)
        .env("GIT_DIR", other.path().join(".git"))
        .env("GIT_WORK_TREE", other.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        fs::read_to_string(temp.path().join("hook-root"))
            .unwrap()
            .trim(),
        fs::canonicalize(temp.path()).unwrap().display().to_string()
    );
}
