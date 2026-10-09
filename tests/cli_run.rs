use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

// `run -- <command>` enters (refresh + print), watches while the command runs,
// then exits with the command's status. It never executes the validation plan.

mod common;
use common::{git_available, run_git};

fn init_git_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("a.txt"), "x\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "init"]);
}

fn run_git_expect_failure(path: &Path, args: &[&str]) {
    let output = common::git_at(path).args(args).output().unwrap();
    assert!(
        !output.status.success(),
        "git {args:?} unexpectedly succeeded"
    );
}

fn assert_run_refuses(path: &Path) {
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(path)
        .args([
            "run",
            "--intent",
            "test",
            "--",
            "sh",
            "-c",
            "touch CHILD_RAN",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("Toimija agent entry").not())
        .stderr(predicate::str::contains(
            "refusing to launch in unsafe Git state",
        ));
    assert!(
        !path.join("CHILD_RAN").exists(),
        "the wrapped child must not run from an unsafe entry packet"
    );
    let session_dir = fs::read_dir(path.join(".toimija/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let marker = fs::read_to_string(session_dir.join("session.json")).unwrap();
    assert!(
        marker.contains("\"status\": \"closed\""),
        "a refused startup must close its session marker: {marker}"
    );
}

#[test]
fn run_exits_with_the_wrapped_command_status() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    // Non-zero status is propagated verbatim.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--intent", "test", "--", "sh", "-c", "exit 7"])
        .assert()
        .code(7);

    // Success is propagated too.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--intent", "test", "--", "sh", "-c", "exit 0"])
        .assert()
        .code(0);
}

#[cfg(unix)]
#[test]
fn run_preserves_signal_termination_as_128_plus_signal() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    // A child killed by SIGTERM (15) should surface as 128 + 15 = 143, so
    // termination is distinguishable from a generic failure (exit 1).
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--intent", "test", "--", "sh", "-c", "kill -TERM $$"])
        .assert()
        .code(143);
}

/// #158: an interrupted run must not leave its session marker `active`.
///
/// Termination is caught from the moment the marker exists, so toimija survives
/// the signal, forwards it to any agent it has launched, and still reaches the
/// teardown that closes the marker. These assert the marker rather than the
/// mechanism: a regression that registers too late, unregisters early, or
/// returns past step 7 brings back the stale-session wall.
#[cfg(unix)]
fn interrupted_run_closes_marker(wait_for_agent: bool) {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let sessions = temp.path().join(".toimija/sessions");
    let launched = temp.path().join("agent-started.txt");

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("toimija"))
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "test",
            "--",
            "sh",
            "-c",
            "touch agent-started.txt; sleep 60",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("toimija runs");

    let until = |ready: &dyn Fn() -> bool, what: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !ready() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };

    // Signalling before the marker is written would prove nothing about
    // closing it.
    let marker = || {
        fs::read_dir(&sessions).ok().and_then(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path().join("session.json"))
                .find(|path| path.is_file())
        })
    };
    until(&|| marker().is_some(), "the session marker");
    if wait_for_agent {
        until(&|| launched.is_file(), "the agent to start");
    }

    // SIGTERM to toimija itself, which is what a closing terminal or a `kill`
    // delivers. Ctrl-C additionally reaches the agent through the tty; here the
    // wrapper is the only target, which is the harder case.
    assert!(
        std::process::Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .expect("kill runs")
            .success()
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if child.try_wait().expect("wait succeeds").is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!("toimija did not exit after SIGTERM");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let marker = marker().expect("the session marker survives");
    let marker: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&marker).unwrap()).unwrap();
    assert_ne!(
        marker["status"], "active",
        "the session marker stayed active after an interrupted run: {marker}"
    );
}

/// Interrupted during entry/watcher arming — the window that was fatal before
/// #158: handlers registered only around the agent, so steps 2-4 killed the
/// wrapper outright.
#[test]
#[cfg(unix)]
fn an_interrupted_run_still_closes_its_session_marker() {
    interrupted_run_closes_marker(false);
}

/// Interrupted while the agent is running — the window supervise_agent already
/// covered. Kept so that consolidating the two registrations cannot quietly
/// drop this one.
#[test]
#[cfg(unix)]
fn interrupting_a_running_agent_still_closes_its_session_marker() {
    interrupted_run_closes_marker(true);
}

#[test]
fn run_enters_then_launches_the_command() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "test",
            "--",
            "sh",
            "-c",
            "echo hi > ran.txt",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Toimija agent entry"))
        .stderr(
            predicate::str::contains("toimija run: watcher armed (native)").or(
                predicate::str::contains("toimija run: watcher armed (poll fallback)").or(
                    predicate::str::contains("toimija run: watcher armed (git polling)"),
                ),
            ),
        );

    // The command ran ...
    assert_eq!(
        fs::read_to_string(temp.path().join("ran.txt"))
            .unwrap()
            .trim(),
        "hi"
    );
    // ... and entry refreshed the live packet.
    assert!(temp.path().join(".toimija/current.md").exists());
    assert!(temp.path().join(".toimija/current.json").exists());
}

#[test]
fn run_captures_recoverable_watcher_diagnostics_off_terminal() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "test",
            "--",
            "sh",
            "-c",
            "mv .git .git-away; sleep 1; mv .git-away .git; sleep 1",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "diagnostic line(s) while the wrapped command ran",
        ))
        .stderr(predicate::str::contains("packet may be stale").not());

    let session_dir = fs::read_dir(temp.path().join(".toimija/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let diagnostics = fs::read_to_string(session_dir.join("diagnostics.log")).unwrap();
    assert!(diagnostics.contains("packet may be stale"), "{diagnostics}");
}

#[test]
fn run_refuses_to_launch_without_intent() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--", "sh", "-c", "touch SHOULD_NOT_RUN"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "missing --intent — every session declares why it exists",
        ))
        // The suggestion echoes the user's own command back, shell-quoted, so
        // it can be copied verbatim.
        .stderr(predicate::str::contains(
            "toimija run --intent \"<why>\" -- sh -c 'touch SHOULD_NOT_RUN'",
        ))
        .stderr(predicate::str::contains("--intent unknown"))
        // The refusal renders exactly once: no WARN telemetry echo above it.
        .stderr(predicate::str::contains("toimija.command.finish").not())
        .stderr(predicate::str::contains("missing --intent").count(1));
    // Refused before launch: the child never ran, no session opened.
    assert!(!temp.path().join("SHOULD_NOT_RUN").exists());
}

#[cfg(unix)]
#[test]
fn run_inlines_the_packet_into_a_known_agents_launch_prompt() {
    use std::os::unix::fs::PermissionsExt;
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    // A stub named `claude`: writes its last positional argument (the composed
    // prompt appended by `run`) so the test can inspect exactly what the agent
    // was launched with.
    let stub = temp.path().join("claude");
    fs::write(
        &stub,
        "#!/bin/sh\nfor last; do :; done\nprintf '%s' \"$last\" > launch-arg\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&stub).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub, permissions).unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "test",
            "--task",
            "paint it beige",
            "--",
            stub.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "delivery: launch prompt with the packet inlined",
        ));

    let arg = fs::read_to_string(temp.path().join("launch-arg")).unwrap();
    assert!(
        arg.starts_with("Your toimija session packet is below."),
        "{arg}"
    );
    assert!(arg.contains("# Toimija Context"), "{arg}");
    assert!(arg.contains("## Workset contract"), "{arg}");
    assert!(arg.contains(".toimija/sessions/"), "{arg}");
    assert!(arg.trim_end().ends_with("Task: paint it beige"), "{arg}");
}

#[cfg(unix)]
#[test]
fn run_inlines_the_packet_for_gemini() {
    use std::os::unix::fs::PermissionsExt;
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let stub = temp.path().join("gemini");
    fs::write(
        &stub,
        "#!/bin/sh\nfor last; do :; done\nprintf '%s' \"$last\" > launch-arg\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&stub).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub, permissions).unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "test",
            "--task",
            "check gemini",
            "--",
            stub.to_str().unwrap(),
        ])
        .assert()
        .success();

    let arg = fs::read_to_string(temp.path().join("launch-arg")).unwrap();
    assert!(arg.contains("# Toimija Context"), "{arg}");
    assert!(arg.trim_end().ends_with("Task: check gemini"), "{arg}");
}

#[cfg(unix)]
#[test]
fn run_inlines_the_packet_for_an_agent_configured_in_repo_yaml() {
    use std::os::unix::fs::PermissionsExt;
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    // `aider` is not a built-in agent; only the repo's `agents:` entry teaches
    // `run` that it accepts the prompt positionally.
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "agents:\n  aider:\n    prompt: positional\n",
    )
    .unwrap();
    let stub = temp.path().join("aider");
    fs::write(
        &stub,
        "#!/bin/sh\nfor last; do :; done\nprintf '%s' \"$last\" > launch-arg\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&stub).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub, permissions).unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "test",
            "--task",
            "refactor the sea gate",
            "--",
            stub.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "delivery: launch prompt with the packet inlined",
        ));

    let arg = fs::read_to_string(temp.path().join("launch-arg")).unwrap();
    assert!(
        arg.starts_with("Your toimija session packet is below."),
        "{arg}"
    );
    assert!(
        arg.trim_end().ends_with("Task: refactor the sea gate"),
        "{arg}"
    );
}

#[cfg(unix)]
#[test]
fn run_inlines_the_packet_behind_a_configured_flag() {
    use std::os::unix::fs::PermissionsExt;
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "agents:\n  flag-agent:\n    prompt: --message\n",
    )
    .unwrap();
    // The configured flag arrives as $1, the composed prompt as $2 — the
    // argv shape a `--message <prompt>` CLI expects.
    let stub = temp.path().join("flag-agent");
    fs::write(
        &stub,
        "#!/bin/sh\nprintf '%s' \"$1\" > flag-arg\nprintf '%s' \"$2\" > launch-arg\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&stub).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub, permissions).unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "test",
            "--task",
            "flag delivery",
            "--",
            stub.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "delivery: launch prompt with the packet inlined",
        ));

    let flag = fs::read_to_string(temp.path().join("flag-arg")).unwrap();
    assert_eq!(flag, "--message");
    let arg = fs::read_to_string(temp.path().join("launch-arg")).unwrap();
    assert!(arg.contains("# Toimija Context"), "{arg}");
    assert!(arg.trim_end().ends_with("Task: flag delivery"), "{arg}");
}

#[test]
fn run_banners_env_var_only_delivery_for_unknown_agents() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run", "--intent", "test", "--task", "t", "--", "sh", "-c", "true",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "delivery: env-var only — not guaranteed for `sh`",
        ))
        .stderr(predicate::str::contains("give it its task yourself"));
}

#[test]
fn run_exit_summary_reports_consumption() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--intent", "fix the roof", "--", "sh", "-c", "true"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "intent:    \"fix the roof\" (declared at launch)",
        ))
        .stderr(predicate::str::contains("expansions: none declared"))
        .stderr(predicate::str::contains("handoff:   none recorded"));
}

#[test]
fn run_refuses_a_merge_in_progress() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    run_git(temp.path(), &["switch", "-c", "feature"]);
    fs::write(temp.path().join("feature.txt"), "feature\n").unwrap();
    run_git(temp.path(), &["add", "feature.txt"]);
    run_git(temp.path(), &["commit", "-m", "feature"]);
    run_git(temp.path(), &["switch", "-"]);
    fs::write(temp.path().join("main.txt"), "main\n").unwrap();
    run_git(temp.path(), &["add", "main.txt"]);
    run_git(temp.path(), &["commit", "-m", "main"]);
    run_git(temp.path(), &["merge", "--no-commit", "feature"]);

    assert_run_refuses(temp.path());
}

#[test]
fn run_refuses_conflicted_files() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    run_git(temp.path(), &["switch", "-c", "feature"]);
    fs::write(temp.path().join("a.txt"), "feature\n").unwrap();
    run_git(temp.path(), &["commit", "-am", "feature"]);
    run_git(temp.path(), &["switch", "-"]);
    fs::write(temp.path().join("a.txt"), "main\n").unwrap();
    run_git(temp.path(), &["commit", "-am", "main"]);
    run_git_expect_failure(temp.path(), &["merge", "feature"]);

    assert_run_refuses(temp.path());
}

#[test]
fn run_refuses_a_rebase_in_progress() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    run_git(temp.path(), &["switch", "-c", "feature"]);
    fs::write(temp.path().join("a.txt"), "feature\n").unwrap();
    run_git(temp.path(), &["commit", "-am", "feature"]);
    run_git(temp.path(), &["switch", "-"]);
    fs::write(temp.path().join("a.txt"), "main\n").unwrap();
    run_git(temp.path(), &["commit", "-am", "main"]);
    run_git(temp.path(), &["switch", "feature"]);
    run_git_expect_failure(temp.path(), &["rebase", "-"]);

    assert_run_refuses(temp.path());
}

#[test]
fn run_writes_a_per_session_packet_and_closes_the_marker() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--intent", "test", "--", "true"])
        .assert()
        .success();

    // Exactly one session directory, with its own packet and a closed marker.
    let sessions = temp.path().join(".toimija/sessions");
    assert!(sessions.is_dir(), "sessions dir should exist");
    let dirs: Vec<_> = fs::read_dir(&sessions)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect();
    assert_eq!(dirs.len(), 1, "exactly one session dir expected");
    let session_dir = &dirs[0];
    assert!(session_dir.join("current.md").exists());
    assert!(session_dir.join("current.json").exists());

    let marker = fs::read_to_string(session_dir.join("session.json")).unwrap();
    assert!(
        marker.contains("\"status\": \"closed\""),
        "marker should be auto-closed on exit: {marker}"
    );
    assert!(
        marker.contains("\"agent\""),
        "marker records the agent argv"
    );

    // The repo-level summary is still written (demoted, last-writer-wins).
    assert!(temp.path().join(".toimija/current.md").exists());
}

#[test]
fn run_intent_reaches_the_session_packet() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    // A wrapped harness infers intent from the packet, so the launch-time
    // declaration must land there — not only on the marker (issue #84).
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "test drive 2",
            "--scope",
            "src/a.rs",
            "--",
            "true",
        ])
        .assert()
        .success();

    let sessions = temp.path().join(".toimija/sessions");
    let session_dir = fs::read_dir(&sessions)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .next()
        .expect("one session dir");
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(session_dir.join("current.json")).unwrap())
            .unwrap();
    assert_eq!(json["session"]["intent"], "test drive 2");
    assert_eq!(json["session"]["scope"][0], "src/a.rs");
    assert_eq!(json["session"]["entry_branch"], json["git"]["branch"]);
    assert_eq!(json["session"]["entry_head"], json["git"]["head_oid"]);
    let md = fs::read_to_string(session_dir.join("current.md")).unwrap();
    assert!(md.contains("## Session"));
    assert!(md.contains("- intent: test drive 2"));

    // The repo-level summary mirrors the same bytes under `run` (D3) — a
    // sessionless refresh afterwards would rewrite it without the block.
    let repo_json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(temp.path().join(".toimija/current.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(repo_json["session"]["intent"], "test drive 2");
}

#[test]
fn run_hands_the_child_a_per_session_handshake() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    // The child captures the handshake env it inherited.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "test",
            "--",
            "sh",
            "-c",
            "printf '%s\\n%s\\n%s\\n%s\\n%s\\n%s\\n%s\\n%s\\n%s' \"$TOIMIJA_RUN\" \"$TOIMIJA_SESSION\" \"$TOIMIJA_LIVE\" \"$TOIMIJA_LIVE_JSON\" \"$TOIMIJA_REPO_LIVE\" \"$TOIMIJA_CURRENT_MD\" \"$TOIMIJA_CURRENT_JSON\" \"$TOIMIJA_REPO_ROOT\" \"$TOIMIJA_RUN_ID\" > env.txt",
        ])
        .assert()
        .success();

    let captured = fs::read_to_string(temp.path().join("env.txt")).unwrap();
    let mut lines = captured.lines();
    assert_eq!(lines.next(), Some("1"), "TOIMIJA_RUN should be 1");
    assert!(
        !lines.next().unwrap_or_default().is_empty(),
        "TOIMIJA_SESSION should be set"
    );
    let live = lines.next().unwrap_or_default();
    assert!(
        live.contains(".toimija/sessions/") && live.ends_with("current.md"),
        "TOIMIJA_LIVE should point at the per-session packet: {live}"
    );
    let live_json = lines.next().unwrap_or_default();
    assert!(
        live_json.contains(".toimija/sessions/") && live_json.ends_with("current.json"),
        "TOIMIJA_LIVE_JSON should point at the per-session packet: {live_json}"
    );
    let repo_live = lines.next().unwrap_or_default();
    assert!(
        repo_live.ends_with(".toimija/current.md"),
        "TOIMIJA_REPO_LIVE should point at the repo-level summary: {repo_live}"
    );
    assert_eq!(lines.next(), Some(live));
    assert_eq!(lines.next(), Some(live_json));
    let repo_root = std::fs::canonicalize(lines.next().unwrap()).unwrap();
    assert_eq!(repo_root, std::fs::canonicalize(temp.path()).unwrap());
    assert_eq!(lines.next(), captured.lines().nth(1));
}

#[cfg(unix)]
#[test]
fn codex_receives_the_same_handshake_in_argv_inside_and_outside_repositories() {
    use std::os::unix::fs::PermissionsExt;
    for in_repo in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        if in_repo {
            init_git_repo(folder.path());
        }
        let stub = folder.path().join("codex");
        fs::write(
            &stub,
            "#!/bin/sh\nprintf '%s\\n' \"$TOIMIJA_SESSION\" \"$TOIMIJA_LIVE\" \"$@\" > captured-handshake\n",
        )
        .unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(folder.path())
            .env("TOIMIJA_STATE_DIR", state.path())
            .env("GIT_CEILING_DIRECTORIES", folder.path().parent().unwrap())
            .args([
                "run",
                "--intent",
                "handshake",
                "--task",
                "check handshake",
                "--",
            ])
            .arg(&stub)
            .args(["exec", "--model", "example"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = fs::read_to_string(folder.path().join("captured-handshake")).unwrap();
        let lines: Vec<_> = stdout.lines().collect();
        for (key, expected) in [("TOIMIJA_SESSION", lines[0]), ("TOIMIJA_LIVE", lines[1])] {
            let prefix = format!("shell_environment_policy.set.{key}=");
            let encoded = lines
                .iter()
                .find_map(|line| line.strip_prefix(&prefix))
                .unwrap();
            assert_eq!(serde_json::from_str::<String>(encoded).unwrap(), expected);
        }
        assert!(
            lines
                .windows(3)
                .any(|w| w == ["exec", "--model", "example"])
        );
        if !in_repo {
            let encoded = lines
                .iter()
                .find_map(|line| {
                    line.strip_prefix("shell_environment_policy.set.TOIMIJA_STATE_DIR=")
                })
                .unwrap();
            assert_eq!(
                serde_json::from_str::<String>(encoded).unwrap(),
                state.path().display().to_string()
            );
        }
        // Codex tools may run in a daemon an earlier session started, whose
        // environment still holds that session's handshake. `set` can only add
        // variables, so what the child loses is set empty for the tools too.
        let mut cleared = vec!["FALSE_SYSTEMS_RESULT_PATH", "FALSE_SYSTEMS_CONTEXT_PATH"];
        if !in_repo {
            cleared.extend([
                "TOIMIJA_LIVE_JSON",
                "TOIMIJA_CURRENT_JSON",
                "TOIMIJA_REPO_ROOT",
                "TOIMIJA_REPO_LIVE",
                "FALSE_SYSTEMS_REPOSITORY_POINT_PATH",
            ]);
        }
        for key in cleared {
            let line = format!("shell_environment_policy.set.{key}=\"\"");
            assert!(lines.contains(&line.as_str()), "{key}: {stdout}");
        }
        let repo_roots: Vec<String> = lines
            .iter()
            .filter_map(|line| line.strip_prefix("shell_environment_policy.set.TOIMIJA_REPO_ROOT="))
            .map(|encoded| serde_json::from_str::<String>(encoded).unwrap())
            .collect();
        assert_eq!(repo_roots.len(), 1, "{stdout}");
        assert_eq!(repo_roots[0].is_empty(), !in_repo, "{stdout}");
    }
}

#[cfg(unix)]
#[test]
fn idle_codex_gets_context_without_a_startup_message() {
    use std::os::unix::fs::PermissionsExt;
    for in_repo in [false, true] {
        for task in [None, Some(""), Some("fix the parser")] {
            let folder = tempfile::tempdir().unwrap();
            let state = tempfile::tempdir().unwrap();
            if in_repo {
                init_git_repo(folder.path());
            }
            // The stub records every invocation. Codex keeps its own
            // instructions when `developer_instructions` is set (the slot is
            // additive), so toimija has nothing to render first: the agent is
            // the only Codex process it starts.
            let stub = folder.path().join("codex");
            fs::write(
                &stub,
                "#!/bin/sh\n{ printf '%s\\n' '--- call'; printf '%s\\n' \"$@\"; } >> captured-args\n",
            )
            .unwrap();
            fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
            let mut command = Command::cargo_bin("toimija").unwrap();
            command
                .current_dir(folder.path())
                .env("TOIMIJA_STATE_DIR", state.path())
                .env("GIT_CEILING_DIRECTORIES", folder.path().parent().unwrap())
                .args([
                    "run",
                    "--intent",
                    "test idle startup",
                    "--briefing",
                    "Keep all rules.\nQuotes: \"x\" and \\ paths.",
                ]);
            if let Some(task) = task {
                command.args(["--task", task]);
            }
            command.arg("--").arg(&stub).assert().success();
            let captured = fs::read_to_string(folder.path().join("captured-args")).unwrap();
            assert_eq!(captured.matches("--- call").count(), 1, "{captured}");
            assert!(!captured.contains("prompt-input"), "{captured}");
            let lines: Vec<_> = captured.lines().skip(1).collect();
            let has_task = task.is_some_and(|task| !task.is_empty());
            let prompt = if has_task {
                assert!(!captured.contains("developer_instructions="));
                assert!(captured.contains("Task: fix the parser"));
                captured.clone()
            } else {
                assert!(
                    lines
                        .chunks(2)
                        .all(|pair| pair.len() == 2 && pair[0] == "-c")
                );
                let encoded = lines
                    .iter()
                    .find_map(|line| line.strip_prefix("developer_instructions="))
                    .unwrap();
                let instructions = serde_json::from_str::<String>(encoded).unwrap();
                // Only toimija's own text: the contract or the outside-repo
                // note first, the briefing last.
                let opening = if in_repo {
                    "Your toimija session packet is below."
                } else {
                    "You are in a toimija session outside any Git repository"
                };
                assert!(instructions.starts_with(opening), "{instructions}");
                assert!(
                    instructions.ends_with("Keep all rules.\nQuotes: \"x\" and \\ paths.\n"),
                    "{instructions}"
                );
                instructions
            };
            assert!(prompt.contains("Keep all rules.\nQuotes: \"x\" and \\ paths."));
            assert_eq!(prompt.contains("# Toimija Context"), in_repo);
            assert_eq!(prompt.contains("outside any Git repository"), !in_repo);
        }
    }
}

/// The facts a wrapped command can read about where it stands: the session's
/// repository point at `FALSE_SYSTEMS_REPOSITORY_POINT_PATH`, and the same
/// point inside the sealed session packet (`session.execution.repository`),
/// which Kelpo reads to bind evidence to a session. The worker.v1 result and
/// context paths are gone with the worker protocol, and cleared even when the
/// outer environment still carries them.
#[test]
fn run_hands_the_child_its_repository_point() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    // Captured outside the repository, so capturing cannot move the point.
    let out = tempfile::tempdir().unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("OUT", out.path())
        // Stale values from an outer environment must not reach the child.
        .env("FALSE_SYSTEMS_RESULT_PATH", "/stale/result.json")
        .env("FALSE_SYSTEMS_CONTEXT_PATH", "/stale/context.json")
        .args([
            "run",
            "--intent",
            "test",
            "--",
            "sh",
            "-c",
            "cp \"$FALSE_SYSTEMS_REPOSITORY_POINT_PATH\" \"$OUT/point.json\" && \
             cp \"$TOIMIJA_LIVE_JSON\" \"$OUT/packet.json\" && \
             printf '%s|%s' \"${FALSE_SYSTEMS_RESULT_PATH-unset}\" \"${FALSE_SYSTEMS_CONTEXT_PATH-unset}\" > \"$OUT/retired.txt\"",
        ])
        .assert()
        .success();

    let read = |name: &str| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(out.path().join(name)).unwrap()).unwrap()
    };
    let point = read("point.json");
    let packet = read("packet.json");
    let id = point["repository"]["id"].as_str().unwrap();
    assert!(id.starts_with("rpoint_"), "{point}");
    assert!(point["repository"]["tree_oid"].is_string(), "{point}");
    assert_eq!(packet["schema"], "toimija-context.v1");
    assert_eq!(packet["session"]["execution"]["repository"]["id"], id);
    assert_eq!(
        fs::read_to_string(out.path().join("retired.txt")).unwrap(),
        "unset|unset"
    );
}

/// The last check before launch runs after the watcher is armed. If it
/// refuses — here because a merge started while `run` was preparing — `run`
/// must close the session it opened, not leave an `active` marker and a
/// stale repository point behind for siblings to trip over.
#[cfg(unix)]
#[test]
fn run_closes_the_session_when_the_last_launch_check_refuses() {
    use std::os::unix::fs::PermissionsExt;
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    // `run` asks `gh` about the branch's pull requests between its first and
    // its last pre-launch check. This `gh` starts a merge while answering.
    let bin = tempfile::tempdir().unwrap();
    let gh = bin.path().join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\ngit rev-parse HEAD > \"$(git rev-parse --git-dir)/MERGE_HEAD\"\necho '[]'\n",
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("PATH", path)
        .env_remove("TOIMIJA_SESSION")
        .args([
            "run",
            "--intent",
            "test",
            "--",
            "sh",
            "-c",
            "touch launched",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unsafe Git state"));

    assert!(!temp.path().join("launched").exists(), "the agent launched");
    let sessions: Vec<_> = fs::read_dir(temp.path().join(".toimija/sessions"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    let marker: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(sessions[0].join("session.json")).unwrap())
            .unwrap();
    assert_eq!(marker["status"], "closed", "{marker}");
    assert!(
        !sessions[0].join("semantic-point.json").exists(),
        "a closed session still publishes a repository point"
    );
}

/// A cycle leg (Ohjaa's `--cycle`, `--cycle-leg`, `--cycle-attempt`) is
/// recorded on the session marker with the repository point it launched at,
/// and `sessions --json` shows it. The same leg at the same point cannot be
/// live twice: a second `run` of it, launched while the first is still
/// running, is refused.
#[test]
fn a_cycle_leg_is_recorded_and_cannot_be_live_twice() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let out = tempfile::tempdir().unwrap();
    let leg = [
        "--cycle",
        "cycle-1",
        "--cycle-leg",
        "code",
        "--cycle-attempt",
        "0",
    ];

    let mut outer = vec!["run", "--intent", "cycle leg"];
    outer.extend(leg);
    outer.extend([
        "--",
        "sh",
        "-c",
        "\"$TOIMIJA_BIN\" run --intent again --cycle cycle-1 --cycle-leg code \
         --cycle-attempt 0 -- true; echo $? > \"$OUT/inner\"",
    ]);
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("OUT", out.path())
        .env("TOIMIJA_BIN", env!("CARGO_BIN_EXE_toimija"))
        .env_remove("TOIMIJA_SESSION")
        .args(&outer)
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(out.path().join("inner")).unwrap().trim(),
        "1",
        "the duplicate leg was not refused"
    );
    let report = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["sessions", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&report).unwrap();
    let actions: Vec<&serde_json::Value> = report["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|session| session.get("cycle_action"))
        .collect();
    assert_eq!(actions.len(), 1, "{report}");
    assert_eq!(actions[0]["cycle"], "cycle-1");
    assert_eq!(actions[0]["leg"], "code");
    assert_eq!(actions[0]["attempt"], 0);
    assert!(
        actions[0]["repository_point"]
            .as_str()
            .unwrap()
            .starts_with("rpoint_"),
        "{report}"
    );
}

#[test]
fn run_closes_the_session_marker_when_the_child_fails_to_launch() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "test",
            "--",
            "__definitely_not_a_toimija_test_command__",
        ])
        .assert()
        .failure();

    let sessions = temp.path().join(".toimija/sessions");
    let dirs: Vec<_> = fs::read_dir(&sessions)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect();
    assert_eq!(dirs.len(), 1, "exactly one session dir expected");
    let marker = fs::read_to_string(dirs[0].join("session.json")).unwrap();
    assert!(
        marker.contains("\"status\": \"closed\""),
        "failed launch should still auto-close the session marker: {marker}"
    );
}

#[test]
fn run_does_not_execute_the_validation_plan() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    // A validation tier that would leave a marker IF Toimija ever ran it.
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test\nvalidation:\n  fast:\n    - \"touch VALIDATION_RAN\"\nboundaries:\n  - stay in scope\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--intent", "test", "--", "true"])
        .assert()
        .success();

    assert!(
        !temp.path().join("VALIDATION_RAN").exists(),
        "run must never execute the configured validation plan"
    );
    // It still entered (refreshed the live packet).
    assert!(temp.path().join(".toimija/current.json").exists());
}

#[test]
fn run_records_a_commit_made_inside_the_session_as_an_observed_effect() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "test",
            "--",
            "sh",
            "-c",
            "git commit -q --allow-empty -m inside",
        ])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("effects:   commit"), "{stderr}");

    let session_dir = fs::read_dir(temp.path().join(".toimija/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let marker: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(session_dir.join("session.json")).unwrap())
            .unwrap();
    assert_eq!(marker["effects_observed"], serde_json::json!(["commit"]));
    // No upstream and (probably) no `gh` here: the answer must say it is
    // partial rather than pretend it looked everywhere.
    assert!(
        marker["effects_coverage"] == "partial" || marker["effects_coverage"] == "complete",
        "{marker}"
    );

    // And a session that only reads records nothing, without lying about it.
    let quiet = tempfile::tempdir().unwrap();
    init_git_repo(quiet.path());
    let assert = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(quiet.path())
        .args([
            "run",
            "--intent",
            "test",
            "--",
            "sh",
            "-c",
            "git status >/dev/null",
        ])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("effects:   none observed"), "{stderr}");
    let session_dir = fs::read_dir(quiet.path().join(".toimija/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let marker = fs::read_to_string(session_dir.join("session.json")).unwrap();
    assert!(!marker.contains("effects_observed"), "{marker}");
}

/// The instructions an idle Codex stub was launched with.
#[cfg(unix)]
fn idle_codex_instructions(briefing: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::tempdir().unwrap();
    init_git_repo(folder.path());
    let stub = folder.path().join("codex");
    fs::write(&stub, "#!/bin/sh\nprintf '%s\\n' \"$@\" > captured-args\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(folder.path())
        .args([
            "run",
            "--intent",
            "briefing shapes",
            "--briefing",
            briefing,
            "--",
        ])
        .arg(&stub)
        .assert()
        .success();
    let captured = fs::read_to_string(folder.path().join("captured-args")).unwrap();
    let encoded = captured
        .lines()
        .find_map(|line| line.strip_prefix("developer_instructions="))
        .unwrap();
    serde_json::from_str::<String>(encoded).unwrap()
}

#[cfg(unix)]
#[test]
fn a_briefing_may_start_with_a_hyphen_and_a_blank_one_is_dropped() {
    if !git_available() {
        return;
    }
    // Launchers write briefings as Markdown, where a list starts with `- `.
    let listed = idle_codex_instructions("- check the receipt first");
    assert!(
        listed.ends_with("\n- check the receipt first\n"),
        "{listed}"
    );

    // A blank briefing is no briefing, like a blank task.
    let blank = idle_codex_instructions("  \n ");
    assert!(!blank.ends_with("\n  \n \n"), "{blank:?}");
    assert!(blank.ends_with("\n"));
    assert!(!blank.trim_end_matches('\n').ends_with(' '), "{blank:?}");
}
