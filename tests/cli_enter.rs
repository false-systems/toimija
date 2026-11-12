use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// `enter` is the agent preflight: it works out the packet from Git when asked
// and prints it. These lock in the packet rendering, the JSON form, the
// session it names under `run`, and that it writes nothing at all — no packet
// file to go stale, and never agent policy docs.

mod common;
use common::{git_available, run_git};

fn init_git_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("README.md"), "# Demo\n").unwrap();
    run_git(path, &["add", "README.md"]);
    run_git(path, &["commit", "-m", "initial"]);
}

/// Every path under `root`, relative to it, sorted: what a command wrote.
fn tree(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            out.push(path.strip_prefix(root).unwrap().display().to_string());
            if path.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn enter_renders_the_context_packet() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["enter", "--for-diff", "--budget", "4000"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("# Toimija Context"))
        .stdout(predicate::str::contains("## Git"))
        .stdout(predicate::str::contains("## Validation plan"));
}

#[test]
fn enter_json_is_valid() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["enter", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let value: serde_json::Value =
        serde_json::from_slice(&output).expect("enter --json must emit valid JSON");
    assert_eq!(value["schema"], "toimija-context.v4");
}

/// The packet is worked out when it is asked for and never written: nothing
/// under `.toimija/` can go stale, and the human-owned agent docs are never
/// touched.
#[test]
fn enter_writes_nothing() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let before = tree(temp.path());

    for args in [
        &["enter"][..],
        &["enter", "--json"],
        &["enter", "--for-diff"],
    ] {
        Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(temp.path())
            .args(args)
            .assert()
            .success();
    }
    assert_eq!(tree(temp.path()), before);
    assert!(!temp.path().join(".toimija").exists());
}

/// Under `toimija run`, `enter` names the session: its intent and scope as
/// the marker says now, and the branch it started on, so drift shows.
#[test]
fn enter_inside_a_run_session_names_the_session() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let dir = temp.path().join(".toimija/sessions/100-1");
    fs::create_dir_all(&dir).unwrap();
    // This test process stands in for the session's `run`: alive, so the
    // session is too.
    fs::write(
        dir.join("session.json"),
        serde_json::json!({
            "id": "100-1", "pid": std::process::id(), "agent": ["claude"],
            "worktree": "/repo", "branch": "feat/elsewhere", "head": null,
            "intent": "fix the redirect", "scope": ["src/auth.rs"],
            "started_at": 100,
        })
        .to_string(),
    )
    .unwrap();

    let enter = || {
        Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(temp.path())
            .env("TOIMIJA_SESSION", "100-1")
            .arg("enter")
            .assert()
            .success()
    };
    enter()
        .stdout(predicate::str::contains("## Session\n- id: 100-1\n"))
        .stdout(predicate::str::contains("- intent: fix the redirect\n"))
        .stdout(predicate::str::contains("- declared scope: src/auth.rs\n"))
        .stdout(predicate::str::contains(
            "## Session drift\n- branch: feat/elsewhere → ",
        ));
    assert!(!dir.join("current.md").exists());

    // Another session's id, or none, names no session.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("TOIMIJA_SESSION", "100-2")
        .arg("enter")
        .assert()
        .success()
        .stdout(predicate::str::contains("## Session").not());
}

/// `toimija handover` leaves a note for the next agent in this working copy:
/// no session needed, shown under "Last handover" in every packet worked out
/// here, and replaced by the next one.
#[test]
fn a_handover_is_shown_to_the_next_agent_on_entering() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    init_git_repo(repo);
    let toimija = |args: &[&str]| {
        let output = Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(repo)
            .env_remove("TOIMIJA_SESSION")
            .args(args)
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    // Nothing to say is refused, and nothing is written.
    let (ok, _, stderr) = toimija(&["handover", "--next", "  "]);
    assert!(!ok);
    assert!(stderr.contains("say at least one thing"), "{stderr}");
    assert!(!repo.join(".toimija/handover.json").exists());

    let (ok, stdout, stderr) = toimija(&[
        "handover",
        "--unfinished",
        "the parser's error tests",
        "--next",
        "the lexer is done; start at src/parse.rs",
    ]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("Handover written for the next agent in")
            && stdout.contains(": unfinished, for the next agent"),
        "{stdout}"
    );
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(repo.join(".toimija/handover.json")).unwrap())
            .unwrap();
    let head = String::from_utf8(
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(written["head"], head.trim());
    assert!(written["branch"].is_string(), "{written}");
    assert!(written.get("session").is_none(), "{written}");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(now.abs_diff(written["written_at"].as_u64().unwrap()) < 60);

    let shown = |text: &str| {
        text.contains("## Last handover\n- written: ")
            && text.contains("- unfinished: the parser's error tests\n")
            && text.contains("- for the next agent: the lexer is done; start at src/parse.rs\n")
            && !text.contains("- surprised by:")
    };
    let (ok, entered, stderr) = toimija(&["enter"]);
    assert!(ok, "{stderr}");
    assert!(shown(&entered), "{entered}");
    // Early: before the workset contract and the Git state.
    assert!(entered.find("## Last handover") < entered.find("## Workset contract"));
    let (ok, json, stderr) = toimija(&["enter", "--json"]);
    assert!(ok, "{stderr}");
    let packet: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        packet["handover"]["next"],
        "the lexer is done; start at src/parse.rs"
    );

    // The next handover replaces it whole.
    let (ok, _, stderr) = toimija(&["handover", "--surprised", "builds go elsewhere"]);
    assert!(ok, "{stderr}");
    let (_, entered, _) = toimija(&["enter"]);
    assert!(
        entered.contains("- surprised by: builds go elsewhere\n")
            && !entered.contains("the parser's error tests"),
        "{entered}"
    );
}

/// No handover here: the packet has no such section.
#[test]
fn a_packet_without_a_handover_has_no_section_for_it() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("enter")
        .assert()
        .success()
        .stdout(predicate::str::contains("## Last handover").not());
}
