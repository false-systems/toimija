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
fn snapshot_excludes_tool_state_but_keeps_the_repo_manifest() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija/sessions")).unwrap();
    fs::create_dir(temp.path().join(".teko")).unwrap();
    fs::write(temp.path().join(".toimija/current.md"), "generated\n").unwrap();
    fs::write(temp.path().join(".toimija/repo.yaml"), "repo: {}\n").unwrap();
    fs::write(
        temp.path().join(".toimija/sessions/session.json"),
        "generated\n",
    )
    .unwrap();
    fs::write(temp.path().join(".teko/teko.redb"), "generated\n").unwrap();

    let tree = snapshot_tree(temp.path());
    let listing = git_stdout(temp.path(), &["ls-tree", "-r", "--name-only", &tree]);
    assert!(listing.contains(".toimija/repo.yaml"), "{listing}");
    assert!(!listing.contains(".toimija/current.md"), "{listing}");
    assert!(!listing.contains(".toimija/sessions/"), "{listing}");
    assert!(!listing.contains(".teko/"), "{listing}");
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

fn toimija(path: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(path)
        .args(args)
        .assert()
}

#[test]
fn a_leased_snapshot_survives_gc_and_release_lets_it_go() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    // An untracked file makes the snapshot tree unique — not reachable via HEAD,
    // so only the lease ref keeps it alive.
    fs::write(temp.path().join("scratch.txt"), "x\n").unwrap();

    let assert = toimija(temp.path(), &["snapshot", "--lease", "--json"]).success();
    let lease: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    let id = lease["id"].as_str().unwrap().to_string();
    let tree = lease["tree"].as_str().unwrap().to_string();

    // Leased: the tree survives aggressive gc (the ref pins it).
    git_stdout(temp.path(), &["gc", "--prune=now"]);
    assert_eq!(
        git_stdout(temp.path(), &["cat-file", "-t", &tree]).trim(),
        "tree"
    );

    // Released: gc may now reclaim it (nothing else references the unique tree).
    toimija(temp.path(), &["snapshot", "--release", &id]).success();
    git_stdout(temp.path(), &["gc", "--prune=now"]);
    let after = std::process::Command::new("git")
        .args(["cat-file", "-t", &tree])
        .current_dir(temp.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        !after.status.success(),
        "released snapshot tree should be reclaimable"
    );
}

#[test]
fn refuses_when_a_path_is_both_staged_and_worktree_modified() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    // Stage new content, then modify the worktree on top: the index and the disk
    // now disagree, so a single tree cannot represent the token unambiguously.
    fs::write(temp.path().join("a.txt"), "staged\n").unwrap();
    run_git(temp.path(), &["add", "a.txt"]);
    fs::write(temp.path().join("a.txt"), "worktree\n").unwrap();

    toimija(temp.path(), &["snapshot"]).failure();
}

#[test]
fn a_tracked_file_that_matches_gitignore_stays_in_the_tree() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    // Commit a file, then start ignoring its pattern: it is now tracked AND
    // gitignore-matched. It must still appear in the materialized tree.
    fs::write(temp.path().join("kept.log"), "keep me\n").unwrap();
    run_git(temp.path(), &["add", "kept.log"]);
    run_git(temp.path(), &["commit", "-m", "add kept.log"]);
    fs::write(temp.path().join(".gitignore"), "*.log\n").unwrap();
    run_git(temp.path(), &["add", ".gitignore"]);
    run_git(temp.path(), &["commit", "-m", "ignore logs"]);

    let tree = snapshot_tree(temp.path());
    let listing = git_stdout(temp.path(), &["cat-file", "-p", &tree]);
    assert!(
        listing.contains("kept.log"),
        "tracked-but-ignored file was dropped from the tree: {listing}"
    );
}

#[test]
fn inherited_git_location_cannot_redirect_snapshot_objects_or_refs() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let repo_a = temp.path().join("a");
    let repo_b = temp.path().join("b");
    fs::create_dir_all(&repo_a).unwrap();
    fs::create_dir_all(&repo_b).unwrap();
    init_repo(&repo_a);
    init_repo(&repo_b);
    fs::write(repo_a.join("only-a.txt"), "snapshot me\n").unwrap();

    let leaked_index = repo_b.join("leaked.index");
    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(&repo_a)
        .env("GIT_DIR", repo_b.join(".git"))
        .env("GIT_WORK_TREE", &repo_b)
        .env("GIT_INDEX_FILE", &leaked_index)
        .args(["snapshot", "--lease", "--json"])
        .assert()
        .success();
    let lease: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    let id = lease["id"].as_str().unwrap();
    let tree = lease["tree"].as_str().unwrap();
    let snapshot_ref = format!("refs/toimija/snapshots/{id}");

    let listing = git_stdout(&repo_a, &["cat-file", "-p", tree]);
    assert!(listing.contains("only-a.txt"), "{listing}");
    assert_eq!(
        git_stdout(&repo_a, &["rev-parse", &snapshot_ref]).trim(),
        tree
    );
    assert!(
        !common::git_at(&repo_b)
            .args(["cat-file", "-e", tree])
            .output()
            .unwrap()
            .status
            .success(),
        "snapshot objects must not be written to the inherited GIT_DIR"
    );
    assert!(
        !common::git_at(&repo_b)
            .args(["rev-parse", "--verify", &snapshot_ref])
            .output()
            .unwrap()
            .status
            .success(),
        "snapshot refs must not be written to the inherited GIT_DIR"
    );
    assert!(
        !leaked_index.exists(),
        "the inherited scratch index path must not be used"
    );
}
