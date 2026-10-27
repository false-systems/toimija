//! Regression test for the fixture git-environment leak.
//!
//! When the suite runs under git's own pre-commit hook, git exports `GIT_DIR`
//! (and friends) pointing at the real repo. A fixture's `git init` that inherits
//! those would mutate the ambient repo instead of its tempdir — once observed
//! flipping `core.bare = true` and bricking a sibling worktree mid-session.
//!
//! `common::run_git` builds its command through `git_at`, which removes those
//! redirecting variables. This test asserts the scrub on the command itself —
//! deterministic, with no global-env mutation (which is `unsafe` on edition
//! 2024 and would race parallel tests anyway).

mod common;

use std::path::Path;

#[test]
fn git_at_scrubs_the_redirecting_environment() {
    let command = common::git_at(Path::new("."));
    // `env_remove` shows up in `get_envs()` as a `(key, None)` override.
    let scrubbed: Vec<String> = command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .collect();

    // The variable that actually caused the corruption, plus the rest of the
    // redirecting family, must all be removed.
    for var in [
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
    ] {
        assert!(
            scrubbed.contains(&var.to_string()),
            "{var} must be scrubbed so fixtures cannot escape to the ambient repo"
        );
    }
}

/// A repository whose `trunk` is the base and whose `work` branch adds one
/// file, the change not yet on `trunk`.
fn repository_with_unlanded_work(path: &Path) {
    common::run_git(path, &["init", "-q", "-b", "trunk"]);
    common::run_git(path, &["config", "user.email", "a@example.invalid"]);
    common::run_git(path, &["config", "user.name", "Fixture A"]);
    std::fs::write(path.join("keep.txt"), "keep\n").unwrap();
    common::run_git(path, &["add", "."]);
    common::run_git(path, &["commit", "-q", "-m", "init"]);
    common::run_git(path, &["switch", "-q", "-c", "work"]);
    std::fs::write(path.join("a.txt"), "alpha\n").unwrap();
    common::run_git(path, &["add", "."]);
    common::run_git(path, &["commit", "-q", "-m", "add a"]);
}

/// Git finds its repository from `GIT_DIR` and friends before the working
/// directory, so a value toimija inherits (from a hook, a wrapper, a test
/// that set it) would point every git it starts at another repository, and at
/// that repository's config. Each child git is pinned to the directory toimija
/// runs in, whatever it inherits. Here the inherited variables are set on
/// toimija's own command line, for that one process: never process-wide,
/// which once leaked a fixture's identity into 80 commits on `main`.
#[test]
fn a_child_git_never_sees_the_repository_its_environment_names() {
    if !common::git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    repository_with_unlanded_work(&a);
    // B: another repository, with config A's git would show if it read it.
    common::run_git(&b, &["init", "-q", "-b", "other"]);
    common::run_git(&b, &["config", "core.abbrev", "20"]);
    common::run_git(&b, &["config", "user.name", "Fixture B"]);
    common::run_git(&b, &["config", "user.email", "b@example.invalid"]);
    std::fs::write(b.join("b.txt"), "b\n").unwrap();
    common::run_git(&b, &["add", "."]);
    common::run_git(&b, &["commit", "-q", "-m", "b"]);

    let head = common::git_at(&a)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let head = String::from_utf8(head.stdout).unwrap().trim().to_string();
    let output = assert_cmd::Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(&a)
        .env("GIT_DIR", b.join(".git"))
        .env("GIT_WORK_TREE", &b)
        .env("GIT_INDEX_FILE", b.join(".git/index"))
        .env("GIT_COMMON_DIR", b.join(".git"))
        .args(["landed", "--base", "trunk", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["branch"], "work", "{report}");
    assert_eq!(report["status"], "not_landed", "{report}");
    assert_eq!(report["touched"], 1, "{report}");
    // A's HEAD, abbreviated by A's config, not B's `core.abbrev = 20`.
    let short = report["head"].as_str().unwrap();
    assert!(head.starts_with(short) && short.len() < 20, "{report}");

    let output = assert_cmd::Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(&a)
        .env("GIT_DIR", b.join(".git"))
        .env("GIT_WORK_TREE", &b)
        .env("GIT_INDEX_FILE", b.join(".git/index"))
        .args(["context", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let packet: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(packet["git"]["branch"], "work", "{}", packet["git"]);
    assert_eq!(packet["git"]["head_oid"], head.as_str());
}
