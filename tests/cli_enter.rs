use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// `enter` is the recommended agent preflight: it refreshes the live packet
// (`.toimija/current.*`) and prints it, framed as agent entry. These lock in the
// packet rendering, the JSON form, and that it writes only the live files — never
// agent policy docs.

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
        .stdout(predicate::str::contains("# Toimija Context"))
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
    assert!(value.is_object(), "packet should be a JSON object");
}

#[test]
fn enter_inside_a_run_session_targets_the_session_packet() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let dir = temp.path().join(".toimija/sessions/100-1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("session.json"),
        r#"{
            "id": "100-1", "pid": 1, "agent": ["claude"],
            "worktree": "/repo", "branch": "main", "head": null,
            "intent": null, "scope": [],
            "started_at": 100, "status": "active",
            "closed_at": null, "exit_status": null
        }"#,
    )
    .unwrap();

    // A wrapped agent following the docs' `toimija enter` preflight must
    // update its own session packet (the one $TOIMIJA_LIVE points at), not
    // just the repo-level summary.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("TOIMIJA_SESSION", "100-1")
        .arg("enter")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "also written to .toimija/sessions/100-1/current.md",
        ));
    assert!(dir.join("current.md").exists());
    assert!(temp.path().join(".toimija/current.md").exists());
}

#[test]
fn enter_writes_the_live_packet_but_not_agent_docs() {
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
        .stdout(predicate::str::contains("Toimija agent entry"));

    // enter refreshes the live packet ...
    assert!(temp.path().join(".toimija/current.md").exists());
    assert!(temp.path().join(".toimija/current.json").exists());
    // ... but never writes human-owned agent policy.
    assert!(!temp.path().join("AGENTS.md").exists());
    assert!(!temp.path().join("CLAUDE.md").exists());
}

#[test]
fn enter_succeeds_when_automatic_cleanup_fails() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let dir = temp.path().join(".toimija/sessions/old-closed");
    fs::create_dir_all(&dir).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(
        dir.join("session.json"),
        format!(
            r#"{{
                "id": "old-closed", "pid": 1, "agent": ["codex"],
                "worktree": "/repo", "branch": "main", "head": null,
                "intent": null, "scope": [], "started_at": {started},
                "status": "closed", "closed_at": {closed}, "exit_status": 0
            }}"#,
            started = now - 9 * 24 * 60 * 60,
            closed = now - 8 * 24 * 60 * 60,
        ),
    )
    .unwrap();
    // The journal cannot be opened, forcing the opportunistic sweep to fail.
    fs::create_dir_all(temp.path().join(".toimija/history.jsonl")).unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("enter")
        .assert()
        .success()
        .stdout(predicate::str::contains("Toimija agent entry"));
    assert!(dir.exists(), "failed journaling must preserve the session");
}

#[test]
fn enter_respects_disabled_automatic_cleanup() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let toimija = temp.path().join(".toimija");
    fs::create_dir_all(&toimija).unwrap();
    fs::write(
        toimija.join("repo.yaml"),
        "stewardship:\n  auto_sweep: false\n",
    )
    .unwrap();
    let orphan = toimija.join("current.json.tmp-123-4");
    fs::write(&orphan, b"old").unwrap();
    fs::File::options()
        .write(true)
        .open(&orphan)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60)),
        )
        .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("enter")
        .assert()
        .success();
    assert!(orphan.exists());
}

/// Age a file or directory. A read-only handle is enough: the owner may set
/// times on any descriptor, and directories cannot be opened for writing.
fn set_age(path: &Path, age: std::time::Duration) {
    fs::File::open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(SystemTime::now() - age))
        .unwrap();
}

/// A repository can commit `.toimija/runs` (or `.toimija/sessions`, or
/// `.toimija` itself) as a symlink. The footprint sweep deletes what it finds
/// there, so following that link would delete whatever the committer pointed
/// it at. Both the opportunistic sweep in `enter` and an explicit `clean` must
/// leave everything outside the repository alone. (`tests/symlink_escape.rs`
/// covers the writers.)
#[cfg(unix)]
#[test]
fn the_sweep_never_follows_a_committed_symlink_out_of_the_repository() {
    if !git_available() {
        return;
    }
    let ancient = std::time::Duration::from_secs(365 * 24 * 60 * 60);
    for link in [".toimija/runs", ".toimija/sessions", ".toimija"] {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        // Everything a sweep would take, were it inside `.toimija`: an old
        // transcript directory, an old transcript, an old orphaned temp file,
        // and a closed session long past retention.
        let victim_dir = outside.join("victim-dir");
        fs::create_dir_all(&victim_dir).unwrap();
        fs::write(victim_dir.join("keep.txt"), "keep\n").unwrap();
        fs::write(outside.join("victim.jsonl"), "keep\n").unwrap();
        fs::write(outside.join("current.json.tmp-1-1"), "keep\n").unwrap();
        let session_dir = outside.join("100-1");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            session_dir.join("session.json"),
            serde_json::json!({
                "id": "100-1",
                "pid": 1,
                "agent": ["claude"],
                "branch": null,
                "started_at": 100,
                "status": "closed",
                "closed_at": 100,
            })
            .to_string(),
        )
        .unwrap();
        for path in [
            victim_dir.as_path(),
            &outside.join("victim.jsonl"),
            &outside.join("current.json.tmp-1-1"),
            &session_dir,
        ] {
            set_age(path, ancient);
        }

        let link_path = repo.join(link);
        fs::create_dir_all(link_path.parent().unwrap()).unwrap();
        let depth = Path::new(link).components().count();
        let target = format!("{}outside", "../".repeat(depth));
        std::os::unix::fs::symlink(&target, &link_path).unwrap();
        run_git(&repo, &["add", "-f", link]);
        run_git(&repo, &["commit", "-m", "commit a symlink under .toimija"]);

        // `enter` writes the packet into `.toimija/` itself, so a symlinked
        // `.toimija` is refused outright; below it, only the sweep is at
        // stake, and `enter` carries on with a warning.
        let enter = Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(&repo)
            .arg("enter")
            .assert();
        if link == ".toimija" {
            enter.failure().stderr(predicate::str::contains("symlink"));
        } else {
            enter.success();
        }
        Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(&repo)
            .arg("clean")
            .assert()
            .failure()
            .stderr(predicate::str::contains("refusing to sweep"));

        for survivor in [
            victim_dir.join("keep.txt"),
            outside.join("victim.jsonl"),
            outside.join("current.json.tmp-1-1"),
            session_dir.join("session.json"),
        ] {
            assert!(
                survivor.exists(),
                "{} was deleted through the committed symlink {link} -> {target}",
                survivor.display()
            );
        }
    }
}

/// `toimija handover` leaves a note for the next agent in this working copy
/// (issue #251): no session needed, shown under "Last handover" in every
/// packet built here, the live `.toimija/current.md` included at once, and
/// replaced by the next one.
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

    // The live packet shows it now, before anyone refreshes.
    let shown = |text: &str| {
        text.contains("## Last handover\n- written: ")
            && text.contains("- unfinished: the parser's error tests\n")
            && text.contains("- for the next agent: the lexer is done; start at src/parse.rs\n")
            && !text.contains("- surprised by:")
    };
    let current = fs::read_to_string(repo.join(".toimija/current.md")).unwrap();
    assert!(shown(&current), "{current}");
    let (ok, entered, stderr) = toimija(&["enter"]);
    assert!(ok, "{stderr}");
    assert!(shown(&entered), "{entered}");
    // Early: before the workset contract and the Git state.
    assert!(entered.find("## Last handover") < entered.find("## Workset contract"));
    let (ok, json, stderr) = toimija(&["context", "--json"]);
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
