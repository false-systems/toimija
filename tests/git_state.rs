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
    // AGENTS.md or CLAUDE.md — it only says what they should carry.
    assert!(!temp.path().join("AGENTS.md").exists());
    assert!(!temp.path().join("CLAUDE.md").exists());
}

#[test]
fn init_says_what_the_agent_docs_should_carry() {
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
        .stdout(predicate::str::contains("run `toimija enter` first"));
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
        // Changed surface is sourced from the branch diff, and branch surface is
        // rendered because a merge-base exists.
        .stdout(predicate::str::contains(
            "source: branch diff since merge-base",
        ))
        .stdout(predicate::str::contains("## Branch surface"))
        .stdout(predicate::str::contains("files changed since merge-base:"));
}

#[test]
fn branch_surface_uses_trunk_not_feature_upstream() {
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
fn context_reads_the_changed_file_then_its_companion_test_first() {
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
        .stdout(predicate::str::contains(
            "## Read first\n- README.md (important doc)\n- src/foo.rs (changed)\n- tests/foo.rs (matching test heuristic)\n",
        ));
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
        // No HEAD and no base: branch surface is unavailable and the changed
        // surface has no source to derive from.
        .stdout(predicate::str::contains(
            "No base branch detected. Branch surface is unavailable.",
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
    // working tree and index, not a branch diff.
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
            "No base branch detected. Branch surface is unavailable.",
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
    // has no committed diff. Only an untracked file is present. The surface
    // must say "working tree and index", not branch diff.
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
}

#[cfg(unix)]
#[test]
fn adversarial_git_paths_round_trip_without_forging_markdown() {
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt as _;

    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    let mut names = vec![
        std::ffi::OsString::from("space name.txt"),
        std::ffi::OsString::from("tab\tname.txt"),
        std::ffi::OsString::from("line\n## FORGED HEADING.txt"),
        std::ffi::OsString::from("-leading-dash.txt"),
        std::ffi::OsString::from("unicode-λ.txt"),
        std::ffi::OsString::from("escape\u{1b}[31m.txt"),
    ];
    if cfg!(target_os = "linux") {
        names.push(std::ffi::OsString::from_vec(b"invalid-\xff.txt".to_vec()));
    }
    for name in names {
        fs::write(temp.path().join(name), "x\n").unwrap();
    }

    let json = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["context", "--json"])
        .output()
        .unwrap();
    assert!(json.status.success());
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let paths = value["git"]["untracked_files"].as_array().unwrap();
    for expected in [
        "space name.txt",
        "tab\tname.txt",
        "line\n## FORGED HEADING.txt",
        "-leading-dash.txt",
        "unicode-λ.txt",
        "escape\u{1b}[31m.txt",
    ] {
        assert!(
            paths.iter().any(|path| path == expected),
            "missing {expected:?}: {paths:?}"
        );
    }
    #[cfg(target_os = "linux")]
    assert!(
        paths
            .iter()
            .any(|path| { path["bytes_hex"] == "696e76616c69642dff2e747874" })
    );

    let markdown = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .output()
        .unwrap();
    assert!(markdown.status.success());
    let markdown = String::from_utf8(markdown.stdout).unwrap();
    assert!(markdown.contains(r"tab\tname.txt"), "{markdown}");
    assert!(
        markdown.contains(r"line\n## FORGED HEADING.txt"),
        "{markdown}"
    );
    assert!(!markdown.contains("\n## FORGED HEADING.txt"), "{markdown}");
    assert!(markdown.contains(r"escape\u{1b}[31m.txt"), "{markdown}");
    #[cfg(target_os = "linux")]
    assert!(markdown.contains(r"invalid-\xff.txt"), "{markdown}");
}

#[test]
fn staged_rename_keeps_old_and_new_paths_in_json() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::rename(
        temp.path().join("README.md"),
        temp.path().join("renamed README.md"),
    )
    .unwrap();
    run_git(temp.path(), &["add", "-A"]);

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["context", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let renamed = value["git"]["staged_files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["status"] == "Renamed")
        .expect("one staged rename");
    assert_eq!(renamed["path"], "renamed README.md");
    assert_eq!(renamed["original_path"], "README.md");
    assert_eq!(renamed["similarity"], 100);
}

#[test]
fn conflict_json_keeps_index_stages_for_a_spaced_path() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    fs::write(temp.path().join("conflict path.txt"), "base\n").unwrap();
    run_git(temp.path(), &["add", "conflict path.txt"]);
    run_git(temp.path(), &["commit", "-m", "add conflict path"]);
    run_git(temp.path(), &["switch", "-c", "feature"]);
    fs::write(temp.path().join("conflict path.txt"), "feature\n").unwrap();
    run_git(temp.path(), &["commit", "-am", "feature"]);
    run_git(temp.path(), &["switch", "-"]);
    fs::write(temp.path().join("conflict path.txt"), "main\n").unwrap();
    run_git(temp.path(), &["commit", "-am", "main"]);
    let merge = common::git_at(temp.path())
        .args(["merge", "feature"])
        .output()
        .unwrap();
    assert!(!merge.status.success(), "merge should stop on a conflict");

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["context", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let conflict = &value["git"]["conflict_entries"][0];
    assert_eq!(conflict["path"], "conflict path.txt");
    let stages = conflict["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 3);
    for (index, stage) in stages.iter().enumerate() {
        assert_eq!(stage["stage"], index + 1);
        assert_eq!(stage["mode"], "100644");
        assert_eq!(stage["oid"].as_str().unwrap().len(), 40);
    }
}

fn context_json(path: &Path) -> serde_json::Value {
    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(path)
        .args(["context", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn context_markdown(path: &Path) -> String {
    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(path)
        .arg("context")
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

fn commit_file(path: &Path, name: &str) {
    fs::write(path.join(name), format!("{name}\n")).unwrap();
    run_git(path, &["add", name]);
    run_git(path, &["commit", "-m", name]);
}

#[test]
fn ahead_and_behind_are_counted_against_the_upstream_without_fetching() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let origin = temp.path().join("origin.git");
    let work = temp.path().join("work");
    let other = temp.path().join("other");
    fs::create_dir_all(&work).unwrap();
    init_repo(&work);
    run_git(&work, &["branch", "-M", "main"]);
    run_git(
        temp.path(),
        &[
            "clone",
            "--bare",
            "-q",
            work.to_str().unwrap(),
            "origin.git",
        ],
    );
    run_git(
        &work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    run_git(&work, &["fetch", "-q", "origin"]);
    run_git(&work, &["branch", "--set-upstream-to=origin/main"]);

    let even = context_json(&work);
    assert_eq!(even["git"]["ahead"], 0);
    assert_eq!(even["git"]["behind"], 0);
    assert_eq!(even["git"]["ahead_behind_ref"], "origin/main");

    // One local commit, and someone else's commit on the upstream.
    commit_file(&work, "mine.txt");
    run_git(
        temp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "other"],
    );
    run_git(&other, &["config", "user.email", "toimija@example.invalid"]);
    run_git(&other, &["config", "user.name", "Toimija Test"]);
    commit_file(&other, "theirs.txt");
    run_git(&other, &["push", "-q", "origin", "HEAD:main"]);

    // Before a fetch the upstream has not moved here: toimija never fetches.
    let before_fetch = context_json(&work);
    assert_eq!(before_fetch["git"]["ahead"], 1);
    assert_eq!(before_fetch["git"]["behind"], 0);

    run_git(&work, &["fetch", "-q", "origin"]);
    let after_fetch = context_json(&work);
    assert_eq!(after_fetch["git"]["ahead"], 1);
    assert_eq!(after_fetch["git"]["behind"], 1);
    let markdown = context_markdown(&work);
    assert!(
        markdown.contains("- ahead/behind: 1 ahead, 1 behind origin/main (upstream)"),
        "{markdown}"
    );
}

#[test]
fn without_an_upstream_ahead_and_behind_use_the_remote_default_branch() {
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
    run_git(temp.path(), &["checkout", "-q", "-b", "feature"]);
    commit_file(temp.path(), "a.txt");
    commit_file(temp.path(), "b.txt");

    let packet = context_json(temp.path());
    assert!(packet["git"]["upstream"].is_null());
    assert_eq!(packet["git"]["ahead"], 2);
    assert_eq!(packet["git"]["behind"], 0);
    assert_eq!(packet["git"]["ahead_behind_ref"], "origin/main");
    let markdown = context_markdown(temp.path());
    assert!(
        markdown.contains("- ahead/behind: 2 ahead, 0 behind origin/main (no upstream)"),
        "{markdown}"
    );
}

#[test]
fn ahead_and_behind_are_unknown_with_nothing_to_compare_against() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());

    let packet = context_json(temp.path());
    assert!(packet["git"]["ahead"].is_null());
    assert!(packet["git"]["behind"].is_null());
    let markdown = context_markdown(temp.path());
    assert!(
        markdown.contains("- ahead/behind: unknown — no upstream and no remote default branch"),
        "{markdown}"
    );
}

/// A repository with commits `one` and `two` on `main`, at `root/source`.
fn source_repo(root: &Path) -> std::path::PathBuf {
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    init_repo(&source);
    run_git(&source, &["branch", "-M", "main"]);
    commit_file(&source, "one.txt");
    commit_file(&source, "two.txt");
    source
}

#[test]
fn a_shallow_repository_leaves_ahead_and_behind_unknown() {
    if !git_available() {
        return;
    }
    // Git counts only the history a shallow repository has. After a shallow
    // fetch the upstream's new commit has no parent here, so the commit both
    // sides share is counted as this branch's: `+2 -1` for one commit ahead.
    let temp = tempfile::tempdir().unwrap();
    let source = source_repo(temp.path());
    let url = format!("file://{}", source.display());
    run_git(
        temp.path(),
        &["clone", "-q", "--depth", "1", &url, "shallow"],
    );
    let shallow = temp.path().join("shallow");
    run_git(
        &shallow,
        &["config", "user.email", "toimija@example.invalid"],
    );
    run_git(&shallow, &["config", "user.name", "Toimija Test"]);
    commit_file(&shallow, "mine.txt");
    commit_file(&source, "theirs.txt");
    run_git(&shallow, &["fetch", "-q", "--depth", "1"]);

    let packet = context_json(&shallow);
    assert!(packet["git"]["ahead"].is_null(), "{}", packet["git"]);
    assert!(packet["git"]["behind"].is_null(), "{}", packet["git"]);
    assert_eq!(packet["git"]["ahead_behind_ref"], "origin/main");
    let markdown = context_markdown(&shallow);
    assert!(
        markdown.contains("- ahead/behind: unknown — the repository is shallow"),
        "{markdown}"
    );
}

#[test]
fn a_remote_not_named_origin_is_the_base_and_counts_ahead_and_behind() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let source = source_repo(temp.path());
    // Cloned with its remote named `upstream` (`upstream/HEAD` set), and a
    // remote added by hand, which has no `HEAD` until asked for one.
    run_git(
        temp.path(),
        &[
            "clone",
            "-q",
            "-o",
            "upstream",
            source.to_str().unwrap(),
            "cloned",
        ],
    );
    let added = temp.path().join("added");
    fs::create_dir_all(&added).unwrap();
    init_repo(&added);
    run_git(&added, &["remote", "add", "fork", source.to_str().unwrap()]);
    run_git(&added, &["fetch", "-q", "fork"]);
    run_git(&added, &["checkout", "-q", "-B", "work", "fork/main"]);
    run_git(&added, &["branch", "--unset-upstream"]);

    for (repo, remote) in [(temp.path().join("cloned"), "upstream"), (added, "fork")] {
        run_git(&repo, &["config", "user.email", "toimija@example.invalid"]);
        run_git(&repo, &["config", "user.name", "Toimija Test"]);
        run_git(&repo, &["checkout", "-q", "-b", "feature"]);
        commit_file(&repo, "mine.txt");
        let base = format!("{remote}/main");

        let packet = context_json(&repo);
        assert_eq!(packet["git"]["base"], base.as_str(), "{}", packet["git"]);
        assert_eq!(packet["git"]["ahead"], 1, "{}", packet["git"]);
        assert_eq!(packet["git"]["behind"], 0, "{}", packet["git"]);
        assert_eq!(packet["git"]["ahead_behind_ref"], base.as_str());
        let markdown = context_markdown(&repo);
        assert!(
            markdown.contains(&format!(
                "- ahead/behind: 1 ahead, 0 behind {base} (no upstream)"
            )),
            "{markdown}"
        );
    }
}

#[test]
fn init_ignores_the_packet_so_the_first_refresh_leaves_the_tree_clean() {
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
        .stdout(predicate::str::contains(".toimija/.gitignore"));
    // What init writes is committed with the repo; what refresh writes is not.
    run_git(temp.path(), &["add", ".toimija"]);
    run_git(temp.path(), &["commit", "-m", "adopt toimija"]);

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("refresh")
        .assert()
        .success();

    let status = common::git_at(temp.path())
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&status.stdout), "");
    let packet: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(temp.path().join(".toimija/current.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(packet["git"]["dirty"], false, "{}", packet["git"]);

    // A second init keeps the file as it is.
    let first = fs::read_to_string(temp.path().join(".toimija/.gitignore")).unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(temp.path().join(".toimija/.gitignore")).unwrap(),
        first
    );
}

/// The repo owner's "read this first" (`context.read_first` and
/// `important_docs`) always shows, first, however wide the branch: the
/// changed files fill only what is left of the list's cap.
#[test]
fn read_first_keeps_the_configured_docs_first_on_a_wide_branch() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    run_git(temp.path(), &["branch", "-M", "main"]);
    fs::create_dir_all(temp.path().join("docs")).unwrap();
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(temp.path().join("docs/architecture.md"), "# Arch\n").unwrap();
    fs::write(temp.path().join("AGENTS.md"), "# Agents\n").unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: wide\n  role: fixture\ncontext:\n  read_first:\n    - docs/architecture.md\n  important_docs:\n    - README.md\n    - AGENTS.md\n",
    )
    .unwrap();
    fs::write(temp.path().join(".gitignore"), ".toimija/\n").unwrap();
    run_git(temp.path(), &["add", "."]);
    run_git(temp.path(), &["commit", "-m", "docs"]);

    run_git(temp.path(), &["checkout", "-b", "wide"]);
    fs::create_dir_all(temp.path().join("src")).unwrap();
    for index in 1..=30 {
        fs::write(
            temp.path().join(format!("src/f{index:02}.txt")),
            format!("{index}\n"),
        )
        .unwrap();
    }
    // A configured doc the branch also changes is still listed with the docs.
    fs::write(temp.path().join("README.md"), "# Demo, changed\n").unwrap();
    run_git(temp.path(), &["add", "."]);
    run_git(temp.path(), &["commit", "-m", "thirty files"]);

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let section: Vec<&str> = stdout
        .split("## Read first\n")
        .nth(1)
        .unwrap()
        .lines()
        .take_while(|line| line.starts_with("- "))
        .collect();
    assert_eq!(
        section[..3],
        [
            "- docs/architecture.md (repo config)",
            "- README.md (important doc)",
            "- AGENTS.md (important doc)",
        ],
        "{stdout}"
    );
    assert_eq!(section[3], "- src/f01.txt (changed)", "{stdout}");
    // Twenty-five entries in all, then an honest count of the rest.
    assert_eq!(section.len(), 26, "{stdout}");
    assert_eq!(section[24], "- src/f22.txt (changed)", "{stdout}");
    assert_eq!(section[25], "- ... and 8 more (truncated)", "{stdout}");
}
