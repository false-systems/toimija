use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

// `run` outside any Git repository: a visible session with no
// packet, recorded in the state directory, never in the folder itself.

mod common;
use common::git_available;

/// Everything that could make a folder look like it is inside a repo, or
/// point toimija at one: the hook-exported `GIT_*` redirects, and handshake
/// values inherited from an outer session.
const SCRUBBED: &[&str] = &[
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "TOIMIJA_SESSION",
    "XDG_STATE_HOME",
];

/// `toimija` run in `folder` (not a repo: git may not look above its parent)
/// with its sessions outside repositories kept in `state`.
fn toimija_outside(folder: &Path, state: &Path) -> Command {
    let mut command = Command::cargo_bin("toimija").unwrap();
    command.current_dir(folder);
    for var in SCRUBBED {
        command.env_remove(var);
    }
    command
        .env("GIT_CEILING_DIRECTORIES", folder.parent().unwrap())
        .env("TOIMIJA_STATE_DIR", state);
    command
}

fn outside_sessions(folder: &Path, state: &Path) -> serde_json::Value {
    let output = toimija_outside(folder, state)
        .args(["sessions", "--json", "--outside-repos"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn run_outside_a_repo_opens_a_visible_session_and_leaves_no_folder_behind() {
    if !git_available() {
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let toimija = assert_cmd::cargo::cargo_bin("toimija");
    // The agent declares its intent and lists the sessions while it runs,
    // then reports what it inherited and exits non-zero.
    let script = format!(
        "'{t}' intent 'declared inside' --scope /y > /dev/null && \
         '{t}' sessions --json > during.json && \
         printf '%s' \"${{TOIMIJA_REPO_ROOT-unset}}\" > repo_root && exit 7",
        t = toimija.display()
    );

    toimija_outside(folder.path(), state.path())
        .env("TOIMIJA_REPO_ROOT", "/inherited/from/an/outer/session")
        .args(["run", "--intent", "outside", "--scope", "/x", "--"])
        .args(["sh", "-c", &script])
        .assert()
        .code(7)
        .stderr(predicate::str::contains("is not inside a Git repository"));

    assert!(
        !folder.path().join(".toimija").exists(),
        "a session outside a repo must not leave a .toimija folder where the agent works"
    );
    assert_eq!(
        fs::read_to_string(folder.path().join("repo_root")).unwrap(),
        "unset",
        "an outer session's repo handshake must not leak into this one"
    );
    let folder_path = fs::canonicalize(folder.path()).unwrap();
    let during: serde_json::Value =
        serde_json::from_slice(&fs::read(folder.path().join("during.json")).unwrap()).unwrap();
    let live = &during["sessions"][0];
    assert_eq!(live["liveness"], "active", "{during}");
    assert_eq!(live["worktree"], folder_path.display().to_string());
    assert_eq!(live["intent"], "declared inside");
    assert_eq!(live["scope"], serde_json::json!(["/x", "/y"]));

    // The agent is gone, and so is its session: no marker is left behind.
    let after = outside_sessions(folder.path(), state.path());
    assert_eq!(after["coverage"]["status"], "available", "{after}");
    assert_eq!(after["sessions"], serde_json::json!([]), "{after}");
    let id = live["id"].as_str().unwrap();
    assert!(!state.path().join(".toimija/sessions").join(id).exists());
}

#[cfg(unix)]
#[test]
fn a_known_agent_gets_its_task_and_no_packet_outside_a_repo() {
    use std::os::unix::fs::PermissionsExt;
    if !git_available() {
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let stub = folder.path().join("claude");
    fs::write(
        &stub,
        "#!/bin/sh\nfor last; do :; done\nprintf '%s' \"$last\" > launch-arg\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&stub).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub, permissions).unwrap();

    toimija_outside(folder.path(), state.path())
        .args(["run", "--intent", "t", "--task", "paint it beige", "--"])
        .arg(&stub)
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "delivery: launch prompt (no packet)",
        ));

    let arg = fs::read_to_string(folder.path().join("launch-arg")).unwrap();
    assert!(
        arg.starts_with("You are in a toimija session outside any Git repository"),
        "{arg}"
    );
    assert!(!arg.contains("session packet is below"), "{arg}");
    assert!(arg.trim_end().ends_with("Task: paint it beige"), "{arg}");
}

#[test]
fn run_outside_a_repo_refuses_what_needs_one_before_opening_a_session() {
    if !git_available() {
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    toimija_outside(folder.path(), state.path())
        .args(["run", "--intent", "t", "--for-diff"])
        .args(["--", "sh", "-c", "touch CHILD_RAN"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--for-diff and --budget"));
    assert!(!folder.path().join("CHILD_RAN").exists());
    assert_eq!(
        outside_sessions(folder.path(), state.path())["sessions"],
        serde_json::json!([]),
        "a refused run opens no session"
    );
}

#[test]
fn sessions_outside_a_repo_lists_the_state_directory_and_says_so() {
    if !git_available() {
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let missing = state.path().join("never-created");
    toimija_outside(folder.path(), &missing)
        .arg("sessions")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Sessions outside any Git repository",
        ));
    assert!(
        !missing.exists(),
        "listing sessions must not create the state directory"
    );
}

#[test]
fn an_unknown_agent_outside_a_repo_is_told_its_briefing_was_not_delivered() {
    if !git_available() {
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let toimija = assert_cmd::cargo::cargo_bin("toimija");
    // The agent lists the sessions while it runs: the marker records the loss.
    let script = format!(
        "'{}' sessions --json --outside-repos > during.json",
        toimija.display()
    );

    toimija_outside(folder.path(), state.path())
        .args([
            "run",
            "--intent",
            "outside",
            "--briefing",
            "knowledge",
            "--",
            "sh",
            "-c",
            &script,
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "toimija run: warning: `sh` is not a known agent; not delivered: briefing",
        ));

    let listed: serde_json::Value =
        serde_json::from_slice(&fs::read(folder.path().join("during.json")).unwrap()).unwrap();
    let session = &listed["sessions"][0];
    assert_eq!(
        session["undelivered"],
        serde_json::json!(["briefing"]),
        "{listed}"
    );
}
