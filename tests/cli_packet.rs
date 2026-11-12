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
fn the_packet_reports_a_dirty_repo() {
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
        .args(["enter", "--for-diff", "--budget", "4000"])
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
fn the_json_packet_names_its_schema_and_repo() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::write(temp.path().join("untracked.txt"), "local\n").unwrap();

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["enter", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        value["repo"]["name"].as_str(),
        Some(temp.path().file_name().unwrap().to_string_lossy().as_ref())
    );
    assert_eq!(value["schema"], "toimija-context.v4");
    // Worked out when asked for: nothing to verify it against later.
    for gone in ["seal", "consistency", "refresh_token", "artifact_id"] {
        assert!(value.get(gone).is_none(), "{gone}: {value:#}");
    }
}

#[test]
fn the_json_packet_applies_configured_read_first_and_ignore_rules() {
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
        .args(["enter", "--json"])
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
fn the_json_packet_has_stable_ordering_in_real_repo() {
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
        .args(["enter", "--json"])
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
fn the_budget_truncates_markdown_output() {
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
        .args(["enter", "--budget", "240"])
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
fn enter_outside_git_repo_fails_clearly() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("enter")
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

/// The packet says what the repository is: `repo.name`, `repo.role` and
/// `repo.not_roles`, from `.toimija/repo.yaml`.
#[test]
fn the_json_packet_carries_the_repo_identity() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: widget\n  role: durable task contract\n  not:\n    - inbox\n    - tracker\n",
    )
    .unwrap();

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["enter", "--json"])
        .assert()
        .success();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["repo"]["name"], "widget");
    assert_eq!(value["repo"]["role"], "durable task contract");
    assert_eq!(
        value["repo"]["not_roles"],
        serde_json::json!(["inbox", "tracker"])
    );
}

/// In a repository that does not ignore them, `git status` lists Toimija's
/// own files under `.toimija/`, such as the handover. They are Toimija's, not the repository's
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
    // Nothing ignores `.toimija/`: the handover is visible to Git.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["handover", "--next", "leave files"])
        .assert()
        .success();
    let context = || -> serde_json::Value {
        let output = Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(temp.path())
            .args(["enter", "--json"])
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
        untracked
            .iter()
            .any(|path| path == ".toimija/handover.json"),
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
    // The Markdown counts them apart, beside `dirty: false`.
    let markdown = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("enter")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let markdown = String::from_utf8(markdown).unwrap();
    assert!(markdown.contains("- dirty: false\n"), "{markdown}");
    assert!(markdown.contains("- untracked: 0 (+"), "{markdown}");
    assert!(
        !markdown.contains("- .toimija/handover.json\n"),
        "{markdown}"
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

/// `enter --identity --json` is what a repository says it is, for a tool that
/// asks only that (ohjaa reads each repository's role from it): the
/// `toimija-identity.v1` document, exactly — schema, then the packet's `repo`
/// object, its fields in this order — and nothing else. It names no session,
/// writes nothing, and leaves a live session's marker as it was.
#[test]
fn enter_identity_prints_exactly_the_identity_document_and_touches_nothing() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija/sessions/100-1")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: widget\n  role: durable task contract\n  not:\n    - inbox\n    - tracker\n\
         context:\n  important_docs:\n    - README.md\n    - AGENTS.md\n\
         boundaries:\n  - Do not add network calls.\n",
    )
    .unwrap();
    // A live session (this test process), whose handshake the caller has.
    let marker = temp.path().join(".toimija/sessions/100-1/session.json");
    let raw = serde_json::json!({
        "id": "100-1", "pid": std::process::id(), "agent": ["claude"],
        "worktree": temp.path(), "branch": "main", "intent": "elsewhere",
        "scope": [], "started_at": 100, "last_seen_at": 100,
    })
    .to_string();
    fs::write(&marker, &raw).unwrap();
    let listing = || -> Vec<String> {
        let mut paths: Vec<String> = walk(temp.path())
            .into_iter()
            .map(|path| {
                path.strip_prefix(temp.path())
                    .unwrap()
                    .display()
                    .to_string()
            })
            .collect();
        paths.sort();
        paths
    };
    let before = listing();

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path().join(".toimija"))
        .env("TOIMIJA_SESSION", "100-1")
        .args(["enter", "--identity", "--json"])
        .assert()
        .success();
    assert_eq!(
        String::from_utf8_lossy(&output.get_output().stdout),
        r#"{
  "schema": "toimija-identity.v1",
  "repo": {
    "name": "widget",
    "role": "durable task contract",
    "not_roles": [
      "inbox",
      "tracker"
    ],
    "important_docs": [
      "README.md",
      "AGENTS.md"
    ],
    "boundaries": [
      "Do not add network calls."
    ]
  }
}
"#
    );
    assert_eq!(listing(), before, "enter --identity wrote something");
    assert_eq!(fs::read_to_string(&marker).unwrap(), raw);

    // It is the packet's own `repo` object.
    let packet = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["enter", "--json"])
        .assert()
        .success();
    let packet: serde_json::Value = serde_json::from_slice(&packet.get_output().stdout).unwrap();
    let identity: serde_json::Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(identity["repo"], packet["repo"]);

    // No configuration: the defaults the packet has, `role` null.
    fs::remove_file(temp.path().join(".toimija/repo.yaml")).unwrap();
    let bare = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["enter", "--identity", "--json"])
        .assert()
        .success();
    let bare: serde_json::Value = serde_json::from_slice(&bare.get_output().stdout).unwrap();
    assert!(bare["repo"]["role"].is_null(), "{bare}");
    assert_eq!(
        bare["repo"]["important_docs"],
        serde_json::json!(["README.md"])
    );

    // A configuration that does not parse fails, as the packet does; it needs
    // `--json`, and shapes no packet.
    fs::write(temp.path().join(".toimija/repo.yaml"), "repo: [\n").unwrap();
    for (args, code) in [
        (&["enter", "--identity", "--json"][..], 1),
        (&["enter", "--identity"], 2),
        (&["enter", "--identity", "--json", "--for-diff"], 2),
        (&["enter", "--identity", "--json", "--budget", "100"], 2),
    ] {
        Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(temp.path())
            .args(args)
            .assert()
            .code(code);
    }
}

/// Every path under `dir`, `.git` left out.
fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }
        if path.is_dir() {
            out.extend(walk(&path));
        }
        out.push(path);
    }
    out
}
