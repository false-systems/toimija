use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

// Minimum language packs (issue #136): a repo recognized only by its manifest
// still gets a real packet — kind, inferred test+lint tiers, honest notes.

mod common;
use common::{git_available, run_git};

fn init_repo(path: &Path, files: &[(&str, &str)]) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    for (name, contents) in files {
        if let Some(parent) = Path::new(name).parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(path.join(parent)).unwrap();
        }
        fs::write(path.join(name), contents).unwrap();
    }
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "init"]);
}

fn assert_packet_contains(path: &Path, needles: &[&str]) {
    let mut assertion = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(path)
        .arg("context")
        .assert()
        .success();
    for needle in needles {
        assertion = assertion.stdout(predicate::str::contains(*needle));
    }
}

#[test]
fn go_repo_gets_the_go_minimum_pack() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(
        temp.path(),
        &[("go.mod", "module demo\n"), ("main.go", "package main\n")],
    );
    assert_packet_contains(
        temp.path(),
        &[
            "- kind: Go",
            "go test ./...",
            "go vet ./...",
            "Validation inferred from Go repo shape (minimum language pack)",
        ],
    );
}

#[test]
fn python_repo_gets_the_python_minimum_pack() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(
        temp.path(),
        &[("pyproject.toml", "[project]\nname = \"demo\"\n")],
    );
    assert_packet_contains(temp.path(), &["- kind: Python", "pytest", "ruff check ."]);
}

#[test]
fn node_repo_gets_the_node_minimum_pack() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path(), &[("package.json", "{}\n")]);
    assert_packet_contains(temp.path(), &["- kind: Node", "npm test", "npm run lint"]);
}

#[test]
fn full_adapters_outrank_minimum_packs_in_polyglot_repos() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    // A Rust repo with a package.json (e.g. tooling) is still a Rust repo.
    init_repo(
        temp.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
            ),
            ("package.json", "{}\n"),
        ],
    );
    assert_packet_contains(temp.path(), &["- kind: Rust", "cargo test --workspace"]);
}

#[test]
fn secondary_languages_are_reported_without_changing_the_primary_plan() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(
        temp.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
            ),
            ("services/api/go.mod", "module demo\n"),
            ("web/package.json", "{}\n"),
            ("xtask/Cargo.toml", "[package]\nname = \"xtask\"\n"),
            ("node_modules/pkg/package.json", "{}\n"),
            ("fixtures/elixir_repo/mix.exs", "[]\n"),
            (
                ".toimija/repo.yaml",
                "repo:\n  name: demo\ncontext:\n  safe_to_ignore:\n    - fixtures/\n",
            ),
        ],
    );

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("context")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "- also present: Go (services/api; manifest: services/api/go.mod)",
        ))
        .stdout(predicate::str::contains(
            "- also present: JS/TS (web; manifest: web/package.json)",
        ))
        .stdout(predicate::str::contains("cargo test --workspace"))
        .stdout(predicate::str::contains("go test ./...").not())
        .stdout(predicate::str::contains("npm test").not())
        .stdout(predicate::str::contains("node_modules").not())
        .stdout(predicate::str::contains("fixtures/elixir_repo").not())
        .stdout(predicate::str::contains("xtask/Cargo.toml").not());
}

#[test]
fn repo_config_still_beats_minimum_inference() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo(
        temp.path(),
        &[
            ("go.mod", "module demo\n"),
            (
                ".toimija/repo.yaml",
                "repo:\n  name: demo\nvalidation:\n  replace_inferred: true\n  fast:\n    - make check\n",
            ),
        ],
    );
    assert_packet_contains(
        temp.path(),
        &["- kind: Go", "make check", "- source: repo config"],
    );
}
