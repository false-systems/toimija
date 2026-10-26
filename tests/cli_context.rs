use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

mod common;
use common::{git_available, run_git};

fn init_git_repo(path: &Path) {
    if !git_available() {
        eprintln!("skipping git integration setup: git unavailable");
        return;
    }
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("README.md"), "# Demo\n").unwrap();
    run_git(path, &["add", "README.md"]);
    run_git(path, &["commit", "-m", "initial"]);
}

#[test]
fn context_markdown_reports_dirty_repo() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test fixture\ncontext:\n  important_docs:\n    - README.md\nboundaries:\n  - stay inside fixture\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(temp.path().join("src/lib.rs"), "pub fn demo() {}\n").unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["context", "--for-diff", "--budget", "4000"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Toimija Context"))
        .stdout(predicate::str::contains("## Diff focus"))
        .stdout(predicate::str::contains("### Committed since base"))
        .stdout(predicate::str::contains("### Staged"))
        .stdout(predicate::str::contains("### Unstaged"))
        .stdout(predicate::str::contains("### Untracked"))
        .stdout(predicate::str::contains("### Conflicts"))
        .stdout(predicate::str::contains("src/lib.rs"))
        .stdout(predicate::str::contains("cargo test --workspace"));
}

#[test]
fn context_json_is_valid() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::write(temp.path().join("untracked.txt"), "local\n").unwrap();

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["context", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        value["repo"]["name"].as_str(),
        Some(temp.path().file_name().unwrap().to_string_lossy().as_ref())
    );
    assert_eq!(value["schema"], "toimija-context.v3");
    assert_eq!(value["seal"]["schema"], "toimija-context.v3");
    let mut packet: toimija::context::ContextPacket = serde_json::from_str(&stdout).unwrap();
    assert_eq!(toimija::seal::verify(&mut packet).unwrap(), None);
}

#[test]
fn context_json_applies_configured_read_first_and_ignore_rules() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::create_dir_all(temp.path().join("docs")).unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::create_dir_all(temp.path().join("target/debug")).unwrap();
    fs::write(temp.path().join("docs/architecture.md"), "# Architecture\n").unwrap();
    run_git(temp.path(), &["add", "docs/architecture.md"]);
    run_git(temp.path(), &["commit", "-m", "add architecture doc"]);
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test fixture\ncontext:\n  read_first:\n    - docs/architecture.md\n  safe_to_ignore:\n    - target/\nboundaries:\n  - stay inside fixture\n",
    )
    .unwrap();
    fs::write(temp.path().join("src/lib.rs"), "pub fn demo() {}\n").unwrap();
    fs::write(temp.path().join("target/debug/noise.txt"), "noise\n").unwrap();

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["context", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(strings_at(&value, "/safe_to_ignore"), vec!["target/"]);
    assert!(
        !strings_at(&value, "/changed_surface")
            .iter()
            .any(|path| path.starts_with("target/"))
    );
    assert!(value["read_first"].as_array().unwrap().iter().any(|entry| {
        entry["path"] == "docs/architecture.md" && entry["reason"] == "repo config"
    }));
}

#[test]
fn context_json_has_stable_ordering_in_real_repo() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test fixture\ncontext:\n  important_docs:\n    - README.md\nboundaries:\n  - stay inside fixture\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::create_dir_all(temp.path().join("src/foo")).unwrap();
    fs::create_dir_all(temp.path().join("tests/foo")).unwrap();
    fs::write(temp.path().join("src/z.rs"), "pub fn z() {}\n").unwrap();
    fs::write(temp.path().join("src/foo/bar.rs"), "pub fn bar() {}\n").unwrap();
    fs::write(
        temp.path().join("src/foo/bar_test.rs"),
        "#[test]\nfn bar() {}\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("tests/foo/bar.rs"),
        "#[test]\nfn bar() {}\n",
    )
    .unwrap();
    fs::write(temp.path().join("zz_untracked.txt"), "z\n").unwrap();
    fs::write(temp.path().join("aa_untracked.txt"), "a\n").unwrap();
    run_git(temp.path(), &["add", "src/z.rs"]);

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["context", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(
        strings_at(&value, "/changed_surface"),
        vec![
            ".toimija/repo.yaml",
            "Cargo.toml",
            "aa_untracked.txt",
            "src/foo/bar.rs",
            "src/foo/bar_test.rs",
            "src/z.rs",
            "tests/foo/bar.rs",
            "zz_untracked.txt",
        ]
    );
    assert_eq!(
        strings_at(&value, "/git/staged_files/0/path"),
        vec!["src/z.rs"]
    );
    assert_eq!(
        strings_at(&value, "/git/untracked_files"),
        vec![
            ".toimija/repo.yaml",
            "Cargo.toml",
            "aa_untracked.txt",
            "src/foo/bar.rs",
            "src/foo/bar_test.rs",
            "tests/foo/bar.rs",
            "zz_untracked.txt",
        ]
    );
    assert_eq!(
        read_first_paths(&value),
        vec![
            "README.md",
            ".toimija/repo.yaml",
            "Cargo.toml",
            "aa_untracked.txt",
            "src/foo/bar.rs",
            "src/foo/bar_test.rs",
            "src/z.rs",
            "tests/foo/bar.rs",
            "zz_untracked.txt",
        ]
    );

    let warnings = strings_at(&value, "/warnings");
    let mut sorted = warnings.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(warnings, sorted);
}

#[test]
fn context_budget_truncates_markdown_output() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test fixture\ncontext:\n  important_docs:\n    - README.md\nboundaries:\n  - stay inside fixture\n",
    )
    .unwrap();
    for index in 0..20 {
        fs::write(
            temp.path().join(format!("changed_{index:02}.txt")),
            format!("changed {index}\n"),
        )
        .unwrap();
    }

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["context", "--budget", "240"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Toimija Context"))
        .stdout(predicate::str::contains(
            "Output truncated to fit approximate budget.",
        ));
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("## Workset contract"));
    assert!(stdout.contains("invisible expansion is."));
}

#[test]
fn context_outside_git_repo_fails_clearly() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a Git repository"));
}

fn strings_at(value: &serde_json::Value, pointer: &str) -> Vec<String> {
    match value.pointer(pointer).unwrap() {
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| match item {
                serde_json::Value::String(text) => text.clone(),
                serde_json::Value::Object(object) => object["path"].as_str().unwrap().to_string(),
                other => panic!("expected string or object with path, got {other:?}"),
            })
            .collect(),
        serde_json::Value::String(text) => vec![text.clone()],
        other => panic!("expected string or array at {pointer}, got {other:?}"),
    }
}

fn read_first_paths(value: &serde_json::Value) -> Vec<String> {
    value["read_first"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["path"].as_str().unwrap().to_string())
        .collect()
}

/// Ohjaa learns what each repo is from `toimija context --json`, and reads
/// exactly `repo.name`, `repo.role` and `repo.not_roles` from it.
#[test]
fn context_json_carries_the_repo_identity_ohjaa_reads() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: teko\n  role: durable task contract\n  not:\n    - inbox\n    - tracker\n",
    )
    .unwrap();

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["context", "--json"])
        .assert()
        .success();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["repo"]["name"], "teko");
    assert_eq!(value["repo"]["role"], "durable task contract");
    assert_eq!(
        value["repo"]["not_roles"],
        serde_json::json!(["inbox", "tracker"])
    );
}

/// In a repository without `toimija init`, nothing keeps Git away from
/// Toimija's own files, so `git status` lists every packet, marker and
/// history line under `.toimija/`. They are Toimija's, not the repository's
/// work: the packet still lists them as Git reported them, but they are not
/// in the changed surface or Read first, raise no warning, and leave the
/// state clean. `.toimija/repo.yaml` is the repository's own.
#[test]
fn toimijas_own_files_are_not_the_repositorys_work() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    // No `.toimija/.gitignore`: a run leaves its files visible to Git.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--intent", "leave files", "--", "true"])
        .assert()
        .success();
    let context = || -> serde_json::Value {
        let output = Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(temp.path())
            .args(["context", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    };

    let value = context();
    let untracked = strings_at(&value, "/git/untracked_files");
    assert!(
        untracked.iter().any(|path| path == ".toimija/current.json"),
        "Git lists Toimija's own files here: {untracked:?}"
    );
    assert_eq!(value["git"]["dirty"], false, "{untracked:?}");
    assert!(
        strings_at(&value, "/changed_surface").is_empty(),
        "{value:#}"
    );
    assert!(
        read_first_paths(&value)
            .iter()
            .all(|path| !path.starts_with(".toimija/")),
        "{value:#}"
    );
    assert!(
        strings_at(&value, "/warnings")
            .iter()
            .all(|warning| !warning.starts_with("Untracked files exist")),
        "{value:#}"
    );

    fs::write(temp.path().join("notes.txt"), "the user's\n").unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n",
    )
    .unwrap();
    let value = context();
    assert_eq!(value["git"]["dirty"], true);
    assert_eq!(
        strings_at(&value, "/changed_surface"),
        [".toimija/repo.yaml", "notes.txt"]
    );
}
