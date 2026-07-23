use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

mod common;
use common::{git_available, run_git};

fn init(path: &Path) {
    run_git(path, &["init", "-b", "main"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("README.md"), "base\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "init"]);
}

fn new_spec(path: &Path, id: &str, scope_gate: &str) {
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(path)
        .args([
            "teko",
            "new",
            "--id",
            id,
            "--title",
            "Fix final-only action loss",
            "--objective",
            "Make completion truthful",
            "--goal",
            "regression=regression test exists",
            "--gate",
            &format!("test={scope_gate}"),
            "--completion",
            "validated",
            "--ratify",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("created spec"));
}

fn start(path: &Path, spec: &str, work: &str, scope: &str) {
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(path)
        .args([
            "teko", "start", "--spec", spec, "--id", work, "--scope", scope,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("active work"));
}

#[test]
fn complete_workflow_persists_invalidates_and_refuses_false_closure() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    new_spec(temp.path(), "spec", "test -f changed.txt");
    start(temp.path(), "spec", "work", "changed.txt");

    // Restart boundaries are real: every assertion below is a new process.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1/4 complete"));

    fs::write(temp.path().join("changed.txt"), "one\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "close"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Cannot close"))
        .stderr(predicate::str::contains("regression test exists"))
        .stderr(predicate::str::contains("test -f changed.txt"));

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "verify", "--claim", "regression"])
        .assert()
        .success()
        .stdout(predicate::str::contains("4/4 complete"))
        .stdout(predicate::str::contains("passed on current tree"));

    // A new tree invalidates, but does not erase, the old receipt.
    fs::write(temp.path().join("changed.txt"), "two\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("stale — passed on previous tree"));
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "close"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("passed on tree"))
        .stderr(predicate::str::contains("current tree is"));

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "verify"])
        .assert()
        .success()
        .stdout(predicate::str::contains("4/4 complete"));
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "close", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"work_id\": \"work\""));
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No active Teko"));
}

#[test]
fn one_active_work_and_scope_are_enforced() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    new_spec(temp.path(), "one", "true");
    new_spec(temp.path(), "two", "true");
    start(temp.path(), "one", "work-one", "src");

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "start", "--spec", "two", "--id", "work-two"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already active"));

    fs::write(temp.path().join("README.md"), "outside\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "show", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"scope-violated\""))
        .stdout(predicate::str::contains("README.md"));
}

#[test]
fn drafts_require_explicit_ratification_and_empty_store_is_safe() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "show", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"active\":null"));

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "teko",
            "new",
            "--id",
            "draft",
            "--title",
            "Draft",
            "--objective",
            "wait",
            "--goal",
            "goal",
            "--completion",
            "changed",
        ])
        .assert()
        .success();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "start", "--spec", "draft"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("explicit ratification"));
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["teko", "start", "--spec", "draft", "--ratify"])
        .assert()
        .success();
}

#[cfg(unix)]
#[test]
fn interrupted_task_resumes_in_codex_and_closes_end_to_end() {
    use std::os::unix::fs::PermissionsExt;

    if !git_available() {
        return;
    }
    let repo = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    init(repo.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(repo.path())
        .args([
            "teko",
            "new",
            "--id",
            "daily",
            "--title",
            "Make Teko pleasant for daily work",
            "--objective",
            "Resume bounded work without another explanation",
            "--goal",
            "resume=Codex receives the active work",
            "--non-goal",
            "ticket integrations",
            "--gate",
            "e2e=test -f changed.txt",
            "--completion",
            "validated",
            "--ratify",
        ])
        .assert()
        .success();
    start(repo.path(), "daily", "daily-work", "changed.txt");

    // A genuinely signal-terminated wrapped session must not consume active work.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(repo.path())
        .args(["run", "--", "sh", "-c", "kill -TERM $$"])
        .assert()
        .code(143);

    let capture = tools.path().join("prompt");
    let codex = tools.path().join("codex");
    fs::write(
        &codex,
        format!(
            "#!/bin/sh\nfor last; do :; done\nprintf '%s' \"$last\" > '{}'\n",
            capture.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&codex, permissions).unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(repo.path())
        .args(["run", "--", codex.to_str().unwrap()])
        .assert()
        .success();

    let prompt = fs::read_to_string(capture).unwrap();
    assert!(prompt.contains("# Active Teko Work"), "{prompt}");
    assert!(prompt.contains("Work: `daily-work`"), "{prompt}");
    assert!(
        prompt.contains("Objective: Resume bounded work without another explanation"),
        "{prompt}"
    );
    assert!(
        prompt.contains("`resume` Codex receives the active work"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Non-goals:\n- ticket integrations"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Next: `toimija teko verify --claim resume`"),
        "{prompt}"
    );

    fs::write(repo.path().join("changed.txt"), "done\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(repo.path())
        .args(["teko", "close"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("[obligation-resume]"))
        .stderr(predicate::str::contains(
            "Next: toimija teko verify --claim resume",
        ));
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(repo.path())
        .args(["teko", "verify", "--claim", "resume"])
        .assert()
        .success()
        .stdout(predicate::str::contains("4/4 complete"))
        .stdout(predicate::str::contains("next: toimija teko close"));
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(repo.path())
        .args(["teko", "close"])
        .assert()
        .success()
        .stdout(predicate::str::contains("closed work daily-work"));
}

#[test]
fn teko_subcommand_help_is_self_contained() {
    for (subcommand, expected) in [
        ("new", "Nothing becomes active until `start`"),
        ("start", "remains in `.toimija/teko.redb`"),
        ("show", "every closure blocker"),
        ("verify", "binds passing receipts to the current tree"),
        ("close", "stable blocker ID"),
        ("next", "copy-pasteable command"),
    ] {
        Command::cargo_bin("toimija")
            .unwrap()
            .args(["teko", subcommand, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains(expected).name(subcommand))
            .stdout(predicate::str::contains("Example").name(subcommand));
    }

    Command::cargo_bin("toimija")
        .unwrap()
        .args(["teko", "new", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Required goal as `id=description`",
        ))
        .stdout(predicate::str::contains(
            "Required evidence level: `changed` or `validated`",
        ));
    Command::cargo_bin("toimija")
        .unwrap()
        .args(["teko", "start", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "inferred only when exactly one is activatable",
        ))
        .stdout(predicate::str::contains("Repository-relative write scope"));
}

#[test]
fn session_packet_attests_the_active_teko_contract() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    new_spec(temp.path(), "spec", "test -f changed.txt");
    start(temp.path(), "spec", "work", "changed.txt");
    let session = temp.path().join(".toimija/sessions/100-1");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("session.json"),
        r#"{
            "id": "100-1", "pid": 1, "agent": ["kisko"],
            "worktree": "/repo", "branch": "main", "head": null,
            "intent": null, "scope": [],
            "started_at": 100, "status": "active",
            "closed_at": null, "exit_status": null
        }"#,
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("TOIMIJA_SESSION", "100-1")
        .arg("refresh")
        .assert()
        .success();

    let packet: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(session.join("current.json")).unwrap()).unwrap();
    let teko = &packet["session"]["teko"];
    assert_eq!(teko["work"]["id"], "work");
    assert_eq!(teko["work"]["objective"], "Make completion truthful");
    assert_eq!(teko["work"]["scope"]["write"][0]["path"], "changed.txt");
    assert_eq!(teko["status"]["required_completion"], "validated");
    assert_eq!(
        teko["status"]["gates"][0]["command"]["shell"],
        "test -f changed.txt"
    );
    assert_eq!(
        teko["status"]["obligations"][0]["description"],
        "regression test exists"
    );
    assert!(!teko["status"]["closable"].as_bool().unwrap());
    assert!(
        packet["sauma"]["digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
}
