use assert_cmd::Command;
use std::fs;
use std::path::Path;

// `verify` is the verify-before-read gate: it checks `.toimija/current.json`
// against the live repo and exits non-zero (with reasons) when the packet is
// stale. Read-only; runs no validation, touches no network.

mod common;
use common::{git_at, git_available, run_git};

fn init_repo_with_commit(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("a.txt"), "x\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "init"]);
}

fn toimija(path: &Path) -> Command {
    let mut command = Command::cargo_bin("toimija").unwrap();
    command.current_dir(path);
    command
}

fn active_session(path: &Path, id: &str) -> std::path::PathBuf {
    let dir = path.join(".toimija/sessions").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("session.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": id,
            "pid": std::process::id(),
            "agent": ["codex"],
            "worktree": path.display().to_string(),
            "branch": "main",
            "head": null,
            "intent": null,
            "scope": [],
            "started_at": 1,
            "last_seen_at": 1,
            "status": "active",
            "closed_at": null,
            "exit_status": null
        }))
        .unwrap(),
    )
    .unwrap();
    dir
}

#[test]
fn fresh_packet_verifies_consistent_then_a_change_makes_it_stale() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());

    // refresh writes current.json (with the consistency block).
    toimija(temp.path()).arg("refresh").assert().success();

    // Nothing changed since the refresh -> consistent, exit 0.
    toimija(temp.path()).arg("verify").assert().success();
    let ok = toimija(temp.path())
        .args(["verify", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ok: serde_json::Value = serde_json::from_slice(&ok).unwrap();
    assert_eq!(ok["status"], "verified");
    assert_eq!(ok["algorithm"], "toimija-consistency-v3");
    assert!(ok["token"].as_str().unwrap().starts_with("sha256:"));

    // Touch the working tree -> stale, exit non-zero, with a reason in JSON.
    fs::write(temp.path().join("b.txt"), "new\n").unwrap();
    let assert = toimija(temp.path())
        .args(["verify", "--json"])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["status"], "stale_packet");
    assert!(
        value["packet_token"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(
        value["current_token"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
}

#[test]
fn remote_base_changes_make_the_packet_stale() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    run_git(temp.path(), &["branch", "-M", "feature"]);

    toimija(temp.path()).arg("refresh").assert().success();
    run_git(
        temp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );

    let output = toimija(temp.path())
        .args(["verify", "--json"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["status"], "stale_packet");
}

#[test]
fn verify_accepts_valid_packet_from_older_additive_schema() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    toimija(temp.path()).arg("refresh").assert().success();
    let path = temp.path().join(".toimija/current.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

    // Model an older producer that signed the same packet before additive
    // coverage fields existed. Its digest is valid for its raw artifact; a
    // newer reader must not hash defaults introduced by deserialization.
    value["git"]
        .as_object_mut()
        .unwrap()
        .remove("branch_diff_coverage");
    value["relevance"]
        .as_object_mut()
        .unwrap()
        .remove("history_coverage");
    value.as_object_mut().unwrap().remove("refresh_token");
    let mut envelope = value.as_object_mut().unwrap().remove("sauma").unwrap();
    envelope["envelope"] = serde_json::json!(toimija::sauma::LEGACY_ENVELOPE_SCHEMA);
    envelope["digest"] = serde_json::Value::String(toimija::sauma::digest_value(&value));
    value["sauma"] = envelope;
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    toimija(temp.path())
        .args(["verify", "--json"])
        .assert()
        .success();
}

#[test]
fn verify_rejects_a_packet_after_its_sauma_envelope_is_removed() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    toimija(temp.path()).arg("refresh").assert().success();
    let path = temp.path().join(".toimija/current.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("sauma");
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    let output = toimija(temp.path())
        .args(["verify", "--json"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["status"], "invalid_packet");
    assert_eq!(report["message"], "packet has no Sauma envelope");
}

#[test]
fn verify_rejects_digest_valid_envelopes_bound_to_the_wrong_schema_or_anchor() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    toimija(temp.path()).arg("refresh").assert().success();
    let path = temp.path().join(".toimija/current.json");
    let original = fs::read_to_string(&path).unwrap();

    for (field, replacement, message) in [
        (
            "schema",
            "other-artifact.v1",
            "Sauma envelope schema differs from artifact schema",
        ),
        (
            "anchored_at",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "Sauma envelope anchor differs from packet consistency token",
        ),
    ] {
        let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
        value["sauma"][field] = serde_json::json!(replacement);
        let digest = toimija::sauma::artifact_digest_value(&value);
        value["sauma"]["digest"] = serde_json::json!(digest);
        fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        let output = toimija(temp.path())
            .args(["verify", "--json"])
            .assert()
            .failure()
            .get_output()
            .stdout
            .clone();
        let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(report["status"], "invalid_packet");
        assert_eq!(report["message"], message);
    }
}

#[test]
fn verify_without_a_packet_is_missing_packet() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    // No refresh has run, so there is no packet to trust.
    let assert = toimija(temp.path())
        .args(["verify", "--json"])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["status"], "missing_packet");
}

#[test]
fn ambient_session_verifies_its_packet_not_the_repo_summary() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    toimija(temp.path()).arg("refresh").assert().success();

    let id = "100-1";
    let session = active_session(temp.path(), id);
    let session_packet = session.join("current.json");
    fs::copy(temp.path().join(".toimija/current.json"), &session_packet).unwrap();
    let session_packet = fs::canonicalize(session_packet).unwrap();

    // Make only the repo-level packet fresh. If verify reads that mirror, this
    // incorrectly succeeds; the authoritative session packet must be stale.
    fs::write(temp.path().join("a.txt"), "changed\n").unwrap();
    toimija(temp.path()).arg("refresh").assert().success();
    let assert = toimija(temp.path())
        .args(["verify", "--json"])
        .env("TOIMIJA_SESSION", id)
        .env("TOIMIJA_LIVE_JSON", &session_packet)
        .assert()
        .failure();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "stale_packet");
    assert_eq!(value["packet_path"], session_packet.display().to_string());
}

#[test]
fn explicit_packet_wins_and_reports_its_identity() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    toimija(temp.path()).arg("refresh").assert().success();
    let session = active_session(temp.path(), "100-1");
    let session_packet = session.join("current.json");
    fs::copy(temp.path().join(".toimija/current.json"), &session_packet).unwrap();
    let session_packet = fs::canonicalize(session_packet).unwrap();

    let assert = toimija(temp.path())
        .args([
            "verify",
            "--packet",
            ".toimija/sessions/100-1/current.json",
            "--json",
        ])
        .assert()
        .success();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "verified");
    assert_eq!(value["packet_path"], session_packet.display().to_string());
    assert!(
        value["artifact_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(value["token"].as_str().unwrap().starts_with("sha256:"));
}

#[test]
fn explicit_packet_rejects_unrelated_paths() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    let assert = toimija(temp.path())
        .args(["verify", "--packet", "a.txt", "--json"])
        .assert()
        .failure();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "invalid_packet");
}

#[test]
fn tampered_ambient_packet_fails_even_when_repo_summary_is_valid() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    toimija(temp.path()).arg("refresh").assert().success();
    let id = "100-1";
    let session = active_session(temp.path(), id);
    let session_packet = session.join("current.json");
    let mut value: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(temp.path().join(".toimija/current.json")).unwrap(),
    )
    .unwrap();
    value["sauma"]["digest"] = serde_json::json!(
        "sha256:0000000000000000000000000000000000000000000000000000000000000000"
    );
    fs::write(
        &session_packet,
        serde_json::to_string_pretty(&value).unwrap(),
    )
    .unwrap();
    let session_packet = fs::canonicalize(session_packet).unwrap();

    let assert = toimija(temp.path())
        .args(["verify", "--json"])
        .env("TOIMIJA_SESSION", id)
        .env("TOIMIJA_LIVE_JSON", &session_packet)
        .assert()
        .failure();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "sauma_digest_mismatch");
    assert_eq!(value["packet_path"], session_packet.display().to_string());
}

#[test]
fn verify_distinguishes_missing_and_unsupported_consistency() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(temp.path().join(".toimija/current.json"), "{}").unwrap();
    let assert = toimija(temp.path())
        .args(["verify", "--json"])
        .assert()
        .failure();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "missing_consistency");

    toimija(temp.path()).arg("refresh").assert().success();
    let path = temp.path().join(".toimija/current.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    value["consistency"]["algorithm"] = serde_json::json!("other");
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    let assert = toimija(temp.path())
        .args(["verify", "--json"])
        .assert()
        .failure();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "unsupported_consistency_algorithm");
}

#[test]
fn verify_rejects_a_forged_sauma_digest() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    toimija(temp.path()).arg("refresh").assert().success();
    let path = temp.path().join(".toimija/current.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    value["sauma"]["digest"] = serde_json::json!(
        "sha256:0000000000000000000000000000000000000000000000000000000000000000"
    );
    fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    let assert = toimija(temp.path())
        .args(["verify", "--json"])
        .assert()
        .failure();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "sauma_digest_mismatch");
}

#[test]
fn staged_and_deleted_and_renamed_changes_are_stale() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    toimija(temp.path()).arg("refresh").assert().success();

    fs::write(temp.path().join("a.txt"), "changed\n").unwrap();
    run_git(temp.path(), &["add", "a.txt"]);
    assert_stale(temp.path());

    run_git(temp.path(), &["reset", "--hard", "HEAD"]);
    toimija(temp.path()).arg("refresh").assert().success();
    run_git(temp.path(), &["rm", "a.txt"]);
    assert_stale(temp.path());

    run_git(temp.path(), &["reset", "--hard", "HEAD"]);
    toimija(temp.path()).arg("refresh").assert().success();
    run_git(temp.path(), &["mv", "a.txt", "renamed.txt"]);
    assert_stale(temp.path());
}

#[test]
fn staged_index_content_change_is_stale_even_when_worktree_bytes_do_not_change() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let blobs = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());

    fs::write(temp.path().join("a.txt"), "ONE\n\nTWO\n").unwrap();
    fs::write(blobs.path().join("index1.txt"), "x\n\nTWO\n").unwrap();
    let index1 = git_stdout(
        temp.path(),
        &[
            "hash-object",
            "-w",
            blobs.path().join("index1.txt").to_str().unwrap(),
        ],
    );
    run_git(
        temp.path(),
        &["update-index", "--cacheinfo", "100644", &index1, "a.txt"],
    );
    toimija(temp.path()).arg("refresh").assert().success();

    fs::write(blobs.path().join("index2.txt"), "ONE\n\nx\n").unwrap();
    let index2 = git_stdout(
        temp.path(),
        &[
            "hash-object",
            "-w",
            blobs.path().join("index2.txt").to_str().unwrap(),
        ],
    );
    run_git(
        temp.path(),
        &["update-index", "--cacheinfo", "100644", &index2, "a.txt"],
    );

    assert_stale(temp.path());
}

#[test]
fn ignored_file_does_not_change_token() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    fs::write(temp.path().join(".gitignore"), "ignored.log\n").unwrap();
    run_git(temp.path(), &["add", ".gitignore"]);
    run_git(temp.path(), &["commit", "-m", "ignore"]);
    toimija(temp.path()).arg("refresh").assert().success();

    fs::write(temp.path().join("ignored.log"), "ignored\n").unwrap();
    toimija(temp.path())
        .args(["verify", "--json"])
        .assert()
        .success();
}

fn git_stdout(path: &Path, args: &[&str]) -> String {
    let output = git_at(path).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn assert_stale(path: &Path) {
    let assert = toimija(path).args(["verify", "--json"]).assert().failure();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "stale_packet");
}

#[test]
fn an_empty_live_json_handshake_reads_as_unset() {
    // Codex can only add variables to its tools' environment, so `run` clears
    // a handshake variable there by setting it empty; an empty value must
    // mean "not set", not a packet path.
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    let id = "100-1";
    let session = active_session(temp.path(), id);
    toimija(temp.path()).arg("refresh").assert().success();
    fs::copy(
        temp.path().join(".toimija/current.json"),
        session.join("current.json"),
    )
    .unwrap();
    let session_packet = fs::canonicalize(session.join("current.json")).unwrap();

    let assert = toimija(temp.path())
        .args(["verify", "--json"])
        .env("TOIMIJA_SESSION", id)
        .env("TOIMIJA_LIVE_JSON", "")
        .assert()
        .success();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "verified");
    assert_eq!(value["packet_path"], session_packet.display().to_string());
}

fn only_session_id(path: &Path) -> String {
    let mut ids: Vec<String> = fs::read_dir(path.join(".toimija/sessions"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(ids.len(), 1, "{ids:?}");
    ids.pop().unwrap()
}

#[test]
fn verify_refuses_a_packet_that_names_a_closed_session() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    toimija(temp.path())
        .args(["run", "--intent", "gone now", "--", "true"])
        .assert()
        .success();
    let id = only_session_id(temp.path());

    // The session's own packet is consistent with the tree, but the session
    // it briefs has ended: reading it as current would hand over a dead brief.
    let packet = format!(".toimija/sessions/{id}/current.json");
    let assert = toimija(temp.path())
        .env_remove("TOIMIJA_SESSION")
        .args(["verify", "--packet", &packet, "--json"])
        .assert()
        .failure();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "session_not_live");
    assert_eq!(value["session"], id.as_str());
    assert_eq!(value["state"], "closed");

    toimija(temp.path())
        .env_remove("TOIMIJA_SESSION")
        .args(["verify", "--packet", &packet])
        .assert()
        .failure()
        .stderr(predicates::str::contains("session_not_live"))
        .stderr(predicates::str::contains(id.as_str()));
}

#[cfg(unix)]
#[test]
fn verify_refuses_a_repo_packet_left_by_a_killed_session() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    // The agent kills `run` outright: no close, no exit refresh. The repo
    // packet still mirrors the session, whose marker still says active.
    toimija(temp.path())
        .args([
            "run",
            "--intent",
            "killed",
            "--",
            "sh",
            "-c",
            "kill -9 $PPID",
        ])
        .assert()
        .failure();
    let id = only_session_id(temp.path());
    let repo_packet: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(temp.path().join(".toimija/current.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(repo_packet["session"]["id"], id.as_str());

    let assert = toimija(temp.path())
        .env_remove("TOIMIJA_SESSION")
        .args(["verify", "--json"])
        .assert()
        .failure();
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["status"], "session_not_live");
    assert_eq!(value["session"], id.as_str());
    assert_eq!(value["state"], "ended");

    // A sessionless refresh drops the dead brief, and the packet verifies.
    toimija(temp.path()).arg("refresh").assert().success();
    toimija(temp.path())
        .env_remove("TOIMIJA_SESSION")
        .arg("verify")
        .assert()
        .success();
}

#[test]
fn an_agent_whose_run_was_killed_can_refresh_its_way_out() {
    // `toimija run` was SIGKILLed or OOM-killed; the agent lives on with the
    // handshake and the marker still says active. Its pid is provably gone,
    // so the session is over: `refresh` must not write the dead brief back,
    // and the remedy `verify` names has to work.
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_repo_with_commit(temp.path());
    let id = "100-1";
    let dir = active_session(temp.path(), id);
    let mut marker: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("session.json")).unwrap()).unwrap();
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = gone.id();
    gone.wait().unwrap();
    marker["pid"] = dead_pid.into();
    marker["intent"] = "dead brief".into();
    fs::write(
        dir.join("session.json"),
        serde_json::to_string_pretty(&marker).unwrap(),
    )
    .unwrap();

    toimija(temp.path())
        .env("TOIMIJA_SESSION", id)
        .arg("refresh")
        .assert()
        .success();
    let packet: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(temp.path().join(".toimija/current.json")).unwrap(),
    )
    .unwrap();
    assert!(packet["session"].is_null(), "{}", packet["session"]);
    let md = fs::read_to_string(temp.path().join(".toimija/current.md")).unwrap();
    assert!(!md.contains("dead brief"), "{md}");

    toimija(temp.path())
        .env("TOIMIJA_SESSION", id)
        .arg("verify")
        .assert()
        .success();
}
