use assert_cmd::Command;
use std::fs;
use std::path::Path;

// `snapshot` materializes the working tree as an immutable git tree object, so a
// reader can read content *at a version* instead of live disk. The tree oid is a
// content address: identical content -> identical oid; any change -> a new oid.

mod common;
use common::{git_available, run_git};

fn init_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("a.txt"), "hello\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "init"]);
}

fn snapshot_tree(path: &Path) -> String {
    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(path)
        .args(["snapshot", "--json"])
        .assert()
        .success();
    let value: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("snapshot --json is valid JSON");
    value["tree"].as_str().unwrap().to_string()
}

/// `git cat-file` with the redirecting env scrubbed (see tests/common).
fn git_stdout(path: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn snapshot_materializes_a_tree_of_the_worktree_including_untracked() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(temp.path().join("untracked.txt"), "scratch\n").unwrap();

    let tree = snapshot_tree(temp.path());
    // It is a real git tree object.
    assert_eq!(
        git_stdout(temp.path(), &["cat-file", "-t", &tree]).trim(),
        "tree"
    );
    // Listing includes both the tracked and the untracked file.
    let listing = git_stdout(temp.path(), &["cat-file", "-p", &tree]);
    assert!(listing.contains("a.txt"), "{listing}");
    assert!(listing.contains("untracked.txt"), "{listing}");
}

#[test]
fn identical_content_yields_the_same_tree_but_a_change_yields_a_new_one() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());

    let first = snapshot_tree(temp.path());
    // Re-materializing an unchanged tree is deterministic (content-addressed).
    assert_eq!(first, snapshot_tree(temp.path()));

    // A same-size content edit produces a different tree oid.
    fs::write(temp.path().join("a.txt"), "world\n").unwrap();
    assert_ne!(first, snapshot_tree(temp.path()));
}
