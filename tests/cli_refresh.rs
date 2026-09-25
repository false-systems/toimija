use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

// `refresh` recomputes the live packet and writes `.toimija/current.md` +
// `.toimija/current.json`. It is read-only except for those two files.

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

fn refresh(path: &Path) {
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(path)
        .arg("refresh")
        .assert()
        .success()
        .stdout(predicate::str::contains(".toimija/current.md"));
}

fn write_active_marker(repo: &Path, id: &str) {
    let dir = repo.join(".toimija/sessions").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("session.json"),
        format!(
            r#"{{
                "id": "{id}", "pid": 1, "agent": ["claude"],
                "worktree": "/repo", "branch": "main", "head": null,
                "intent": null, "scope": [],
                "started_at": 100, "status": "active",
                "closed_at": null, "exit_status": null
            }}"#
        ),
    )
    .unwrap();
}

#[test]
fn refresh_inside_a_run_session_targets_the_session_packet() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_active_marker(temp.path(), "100-1");

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("TOIMIJA_SESSION", "100-1")
        .arg("refresh")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            ".toimija/sessions/100-1/current.md",
        ));

    // The session packet is written, and the repo-level summary is mirrored.
    let session_md = temp.path().join(".toimija/sessions/100-1/current.md");
    assert!(session_md.exists(), "session packet should be written");
    assert!(temp.path().join(".toimija/current.md").exists());
    let session_json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(temp.path().join(".toimija/sessions/100-1/current.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(session_json["schema"], "toimija-context.v1");
    let artifact_id = session_json["artifact_id"].as_str().unwrap();
    assert!(
        fs::read_to_string(&session_md)
            .unwrap()
            .contains(&format!("- artifact-id: {artifact_id}")),
        "the Markdown and JSON session packets must identify the same refresh"
    );
    assert_eq!(
        session_json["sauma"]["producer"]["run"].as_str(),
        Some("100-1")
    );
    // Refreshing one's own packet is activity: the heartbeat is stamped.
    let marker: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(temp.path().join(".toimija/sessions/100-1/session.json")).unwrap(),
    )
    .unwrap();
    assert!(
        marker["last_seen_at"].is_u64(),
        "heartbeat should be stamped"
    );
}

#[test]
fn refresh_with_a_session_env_but_no_marker_stays_repo_level() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    // An id inherited from a session in another repo finds no marker here:
    // the handshake must never steer writes into the wrong repo.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("TOIMIJA_SESSION", "100-1")
        .arg("refresh")
        .assert()
        .success()
        .stdout(predicate::str::contains(".toimija/current.md"));
    assert!(!temp.path().join(".toimija/sessions").exists());
}

#[test]
fn refresh_with_a_corrupt_marker_warns_and_stays_repo_level() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let dir = temp.path().join(".toimija/sessions/100-1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("session.json"), "not json").unwrap();

    // Issue #161: unreadable ≠ missing. The refresh still succeeds and stays
    // repo-level — writes must not target a session whose state is unknown —
    // but the broken marker is warned about instead of being silently treated
    // as "no session".
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("TOIMIJA_SESSION", "100-1")
        .arg("refresh")
        .assert()
        .success()
        .stdout(predicate::str::contains(".toimija/current.md"))
        .stderr(predicate::str::contains(
            "toimija.session.marker_unreadable",
        ));
    assert!(!dir.join("current.md").exists());
}

#[test]
fn refresh_with_a_mismatched_marker_id_warns_and_stays_repo_level() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    // A parseable marker whose recorded id disagrees with its directory:
    // broken, not missing — warned about, never targeted for writes.
    write_active_marker(temp.path(), "200-2");
    let dir = temp.path().join(".toimija/sessions/100-1");
    fs::create_dir_all(&dir).unwrap();
    fs::rename(
        temp.path().join(".toimija/sessions/200-2/session.json"),
        dir.join("session.json"),
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("TOIMIJA_SESSION", "100-1")
        .arg("refresh")
        .assert()
        .success()
        .stdout(predicate::str::contains(".toimija/current.md"))
        .stderr(predicate::str::contains("toimija.session.marker_mismatch"));
    assert!(!dir.join("current.md").exists());
}

#[test]
fn session_refresh_with_an_unreadable_marker_still_writes_the_packet() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let dir = temp.path().join(".toimija/sessions/100-1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("session.json"), "not json").unwrap();

    // Issue #161: the marker went unreadable mid-session. The session packet
    // is still written (best-effort, never a failed refresh) — it just loses
    // its session block; the warning is asserted at the CLI layer above.
    let refreshed = toimija::refresh::refresh_session(
        &toimija::command::ProcessCommandRunner,
        temp.path(),
        false,
        None,
        &dir,
    )
    .unwrap();
    assert!(
        refreshed.packet.session.is_none(),
        "an unreadable marker must drop the session block, not fail the refresh"
    );
    assert!(dir.join("current.md").exists());
}

#[test]
fn refresh_writes_current_md_and_json() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    refresh(temp.path());

    let md = fs::read_to_string(temp.path().join(".toimija/current.md")).unwrap();
    assert!(md.contains("# Toimija Context"));

    let json = fs::read_to_string(temp.path().join(".toimija/current.json")).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&json).expect("current.json must be valid JSON");
    assert!(value.is_object());
    assert_eq!(value["schema"], "toimija-context.v1");
    let artifact_id = value["artifact_id"].as_str().unwrap();
    assert!(
        md.contains(&format!("- artifact-id: {artifact_id}")),
        "the Markdown and JSON repo packets must identify the same refresh"
    );
    assert_eq!(value["consistency"]["algorithm"], "toimija-consistency-v2");
    assert_eq!(value["sauma"]["envelope"], "sauma-envelope.v2");
    assert_eq!(value["sauma"]["schema"], "toimija-context.v1");
    assert_eq!(value["sauma"]["producer"]["tool"], "toimija");
    assert_eq!(value["sauma"]["producer"]["run"], serde_json::Value::Null);
    assert_eq!(value["sauma"]["anchored_at"], value["consistency"]["token"]);
    assert!(
        value["sauma"]["digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(value["sauma"]["provenance"][0]["kind"], "git");
    assert!(
        value["consistency"]["index_fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(
        value["consistency"]["worktree_fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(
        value["consistency"]["untracked_fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(
        value["consistency"]["token"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(value["consistency"]["head"]["kind"].is_string());
    assert!(value["consistency"]["git_operation"]["kind"].is_string());
    assert!(value["consistency"]["conflict_state"]["has_conflicts"].is_boolean());

    let mut packet: toimija::context::ContextPacket =
        serde_json::from_str(&json).expect("current.json must match the packet type");
    assert_eq!(
        toimija::sauma::verify(&mut packet).unwrap(),
        None,
        "pretty on-disk current.json must reproduce its sauma digest via canonical serialization"
    );
}

#[test]
fn refresh_updates_a_stale_packet_after_git_state_changes() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    refresh(temp.path());
    let first = fs::read_to_string(temp.path().join(".toimija/current.json")).unwrap();
    assert!(
        !first.contains("late_arrival.rs"),
        "new file should not be present before it exists"
    );

    // Change the working-tree state, then refresh again.
    fs::write(temp.path().join("late_arrival.rs"), "// new\n").unwrap();
    refresh(temp.path());
    let second = fs::read_to_string(temp.path().join(".toimija/current.json")).unwrap();

    assert_ne!(first, second, "packet should change when Git state changes");
    assert!(
        second.contains("late_arrival.rs"),
        "refreshed packet should reflect the new untracked file"
    );
}

#[test]
fn refresh_mutates_nothing_outside_current_files() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let readme_before = fs::read_to_string(temp.path().join("README.md")).unwrap();

    refresh(temp.path());

    // The only writes are the two live files; a pre-existing tracked file is
    // untouched and no agent docs appear.
    assert_eq!(
        readme_before,
        fs::read_to_string(temp.path().join("README.md")).unwrap()
    );
    assert!(!temp.path().join("AGENTS.md").exists());
    assert!(!temp.path().join("CLAUDE.md").exists());

    // Git sees only `.toimija/` as new — nothing tracked was modified.
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&status.stdout);
    for line in status.lines() {
        assert!(
            line.contains(".toimija/"),
            "unexpected change outside .toimija/: {line:?}"
        );
    }
}

#[test]
fn refresh_json_prints_packet_to_stdout() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["refresh", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let value: serde_json::Value =
        serde_json::from_slice(&output).expect("refresh --json must emit valid JSON");
    assert!(value.is_object());
    // And the file was still written.
    assert!(temp.path().join(".toimija/current.json").exists());
}
