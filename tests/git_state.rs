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

fn init_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("README.md"), "# Demo\n").unwrap();
    run_git(path, &["add", "README.md"]);
    run_git(path, &["commit", "-m", "initial"]);
}

fn init_unborn_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
}

#[test]
fn init_creates_config_without_overwrite() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains(".toimija/repo.yaml"))
        // A fresh config prints next-steps guidance to fill identity fields.
        .stdout(predicate::str::contains("Next steps:"));
    let first = fs::read_to_string(temp.path().join(".toimija/repo.yaml")).unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("Refused to overwrite"))
        // Nothing was created, so there is no guidance the second time.
        .stdout(predicate::str::contains("Next steps:").not());
    let second = fs::read_to_string(temp.path().join(".toimija/repo.yaml")).unwrap();
    assert_eq!(first, second);

    // Toimija owns `.toimija/`, not root agent policy. init must not create
    // AGENTS.md or CLAUDE.md — it only suggests preflight text via `snippet`.
    assert!(!temp.path().join("AGENTS.md").exists());
    assert!(!temp.path().join("CLAUDE.md").exists());
}

#[test]
fn init_points_at_the_snippet_for_agent_docs() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("toimija snippet preflight"));
}

#[test]
fn build_artifacts_are_filtered_live_without_config() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    // Create the Toimija config via `toimija init`.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success();
    // A fresh config does not seed safe_to_ignore — build noise is filtered live.
    let config = fs::read_to_string(temp.path().join(".toimija/repo.yaml")).unwrap();
    assert!(
        config.contains("safe_to_ignore: []"),
        "fresh config should not seed safe_to_ignore; got:\n{config}"
    );
    // Nested build output, no safe_to_ignore configured. The built-in artifact
    // filter is applied live, so a nested `target/` is kept out of the changed
    // surface regardless of repo kind.
    fs::create_dir_all(temp.path().join("test_projects/rust_test/target")).unwrap();
    fs::write(
        temp.path().join("test_projects/rust_test/target/junk.o"),
        "noise\n",
    )
    .unwrap();
    fs::write(temp.path().join("real_source.txt"), "keep me\n").unwrap();

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["context", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let surface: Vec<String> = value["changed_surface"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap().to_string())
        .collect();
    assert!(
        surface.iter().any(|p| p == "real_source.txt"),
        "real source should be in the surface: {surface:?}"
    );
    assert!(
        !surface.iter().any(|p| p.contains("target/")),
        "nested target/ build artifacts must be filtered: {surface:?}"
    );
}

#[test]
fn validate_plan_does_not_run_commands() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(
        temp.path().join("mix.exs"),
        "defmodule Demo.MixProject do\nend\n",
    )
    .unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["validate", "--plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("mix test"));
}

#[test]
fn staged_file_appears_in_context() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(temp.path().join("staged.txt"), "staged\n").unwrap();
    run_git(temp.path(), &["add", "staged.txt"]);
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("staged: 1"))
        .stdout(predicate::str::contains("staged.txt"));
}

#[test]
fn committed_branch_change_appears_in_surface_with_clean_tree() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    // Pin the base branch name so discover_base resolves it deterministically.
    run_git(temp.path(), &["branch", "-M", "main"]);
    run_git(temp.path(), &["checkout", "-b", "feature"]);
    fs::write(temp.path().join("feature.txt"), "feature\n").unwrap();
    run_git(temp.path(), &["add", "feature.txt"]);
    run_git(temp.path(), &["commit", "-m", "add feature"]);
    // Working tree is clean: the file is only visible via merge-base..HEAD.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("dirty: false"))
        .stdout(predicate::str::contains("feature.txt"))
        // Changed surface is sourced from the branch diff, and branch intent is
        // rendered because a merge-base exists.
        .stdout(predicate::str::contains(
            "source: branch diff since merge-base",
        ))
        .stdout(predicate::str::contains("## Branch intent"))
        .stdout(predicate::str::contains("files changed since merge-base:"));
}

#[test]
fn branch_intent_uses_trunk_not_feature_upstream() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    run_git(temp.path(), &["branch", "-M", "main"]);
    let origin = temp.path().to_str().unwrap();
    run_git(temp.path(), &["remote", "add", "origin", origin]);
    run_git(
        temp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    run_git(temp.path(), &["checkout", "-b", "feature"]);
    fs::write(temp.path().join("feature.txt"), "feature\n").unwrap();
    run_git(temp.path(), &["add", "feature.txt"]);
    run_git(temp.path(), &["commit", "-m", "add feature"]);
    run_git(
        temp.path(),
        &["update-ref", "refs/remotes/origin/feature", "HEAD"],
    );
    run_git(temp.path(), &["branch", "--set-upstream-to=origin/feature"]);

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("upstream: origin/feature"))
        .stdout(predicate::str::contains("compared against: origin/main"))
        .stdout(predicate::str::contains(
            "files changed since merge-base: 1",
        ))
        .stdout(predicate::str::contains("feature.txt"))
        .stdout(predicate::str::contains("files changed since merge-base: 0").not());
}

#[test]
fn context_surfaces_missing_companion_test_and_risk_action() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    run_git(temp.path(), &["branch", "-M", "main"]);
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::create_dir_all(temp.path().join("tests")).unwrap();
    fs::write(temp.path().join("tests/foo.rs"), "#[test]\nfn foo() {}\n").unwrap();
    run_git(temp.path(), &["add", "Cargo.toml", "tests/foo.rs"]);
    run_git(temp.path(), &["commit", "-m", "add rust test surface"]);

    run_git(temp.path(), &["checkout", "-b", "feature"]);
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(temp.path().join("src/foo.rs"), "pub fn foo() {}\n").unwrap();
    run_git(temp.path(), &["add", "src/foo.rs"]);
    run_git(temp.path(), &["commit", "-m", "add foo source"]);

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("## Coupling"))
        .stdout(predicate::str::contains(
            "src/foo.rs changed, but tests/foo.rs was not",
        ))
        .stdout(predicate::str::contains(
            "companion test exists but is not in this diff",
        ))
        .stdout(predicate::str::contains("## Risk"))
        .stdout(predicate::str::contains(
            "src/foo.rs — added on this branch — new surface; new surface without a changed companion test — consider adding coverage",
        ))
        .stdout(predicate::str::contains("↔").not());
}

#[test]
fn unborn_branch_reports_head_and_merge_base_unavailable() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_unborn_repo(temp.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("HEAD: unavailable"))
        .stdout(predicate::str::contains(
            "HEAD unavailable; branch has no commits yet.",
        ))
        // No HEAD and no base: branch intent is unavailable and the changed
        // surface has no source to derive from.
        .stdout(predicate::str::contains(
            "No base branch detected. Branch intent is unavailable.",
        ))
        .stdout(predicate::str::contains(
            "- unavailable: no merge-base with",
        ))
        .stdout(predicate::str::contains(
            "source: unknown; no HEAD and no local changes",
        ));
}

#[test]
fn detached_head_warns_and_renders() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    run_git(temp.path(), &["checkout", "--detach"]);
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("detached: true"))
        .stdout(predicate::str::contains(
            "Detached HEAD. Broad agent work is risky here.",
        ));
}

#[test]
fn untracked_file_warns() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(temp.path().join("scratch.txt"), "scratch\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("untracked: 1"))
        .stdout(predicate::str::contains(
            "Untracked files exist. They will not be committed unless added.",
        ));
}

#[test]
fn changed_surface_falls_back_to_working_tree_without_a_base() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    // Rename the branch away from main/master so no base candidate resolves.
    // HEAD exists but there is no merge-base, so the surface source is the
    // working tree and index, not branch intent.
    run_git(temp.path(), &["branch", "-M", "wip"]);
    fs::write(temp.path().join("scratch.txt"), "scratch\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains("source: working tree and index"))
        .stdout(predicate::str::contains(
            "No base branch detected. Branch intent is unavailable.",
        ));
}

#[test]
fn working_tree_dirt_at_base_is_not_labelled_branch_diff() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    // A branch sitting at its base: merge-base exists (== HEAD) but the branch
    // has no committed diff. Only an untracked file is present. The surface and
    // the relevance scope must say "working tree and index", not branch diff.
    run_git(temp.path(), &["branch", "-M", "main"]);
    run_git(temp.path(), &["checkout", "-b", "feature"]);
    fs::write(temp.path().join("scratch.txt"), "scratch\n").unwrap();
    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["context", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        value["changed_surface_source"], "working_tree_and_index",
        "no committed branch diff -> working tree, not merge-base diff"
    );
    assert_eq!(
        value["relevance"]["scope"]["source"], "working_tree_and_index",
        "relevance scope must mirror the real surface source"
    );
}
