use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

// `run -- <command>` works out the packet, opens a session, launches the
// command with the packet in its launch prompt, starts no git process while
// the command runs, ends the session when it exits, and exits with its
// status. It never executes the validation plan.

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

/// An interrupted run must not leave its session marker behind.
///
/// Termination is caught from before the marker exists, so toimija survives
/// the signal, forwards it to any agent it has launched, and still reaches the
/// teardown that removes the marker. These assert the marker rather than the
/// mechanism: a regression that registers too late or unregisters early
/// leaves the marker of a session that is over.
#[cfg(unix)]
fn interrupted_run_removes_marker(wait_for_agent: bool) {
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
    // removing it.
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

    assert_eq!(
        marker(),
        None,
        "the session marker stayed behind after an interrupted run"
    );
}

/// Interrupted while the session opens: handlers registered only around the
/// agent would let the signal kill the wrapper outright.
#[test]
#[cfg(unix)]
fn an_interrupted_run_still_removes_its_session_marker() {
    interrupted_run_removes_marker(false);
}

/// Interrupted while the agent is running — the window supervise_agent already
/// covered. Kept so that consolidating the two registrations cannot quietly
/// drop this one.
#[test]
#[cfg(unix)]
fn interrupting_a_running_agent_still_removes_its_session_marker() {
    interrupted_run_removes_marker(true);
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
        .stdout(predicate::str::starts_with("# Toimija Context"))
        .stderr(predicate::str::contains(
            "toimija run: launching `sh` (session ",
        ))
        .stderr(predicate::str::contains("watcher").not());

    // The command ran ...
    assert_eq!(
        fs::read_to_string(temp.path().join("ran.txt"))
            .unwrap()
            .trim(),
        "hi"
    );
    // ... and no packet file was written, and no marker is left.
    assert!(!temp.path().join(".toimija/current.md").exists());
    assert!(!temp.path().join(".toimija/current.json").exists());
    let sessions = temp.path().join(".toimija/sessions");
    assert_eq!(fs::read_dir(&sessions).unwrap().count(), 0);
}

#[test]
fn run_launches_without_an_intent_and_says_none_was_given() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--", "sh", "-c", "touch RAN"])
        .assert()
        .success()
        // Said once, before the agent has the terminal, with how to say it.
        .stderr(
            predicate::str::contains(
                "toimija run: no intent given: other sessions see `unknown` until one is \
                 declared (`toimija intent \"<why>\"`, or `--intent` at launch)",
            )
            .count(1),
        )
        // Missing is not an empty intent declared at launch.
        .stderr(predicate::str::contains("intent:    not given"))
        .stderr(predicate::str::contains("declared at launch").not());
    assert!(temp.path().join("RAN").exists());
}

/// The agent is named right after `run`: no `--`, and what follows the
/// agent is the agent's, even when it looks like one of `run`'s own flags.
#[test]
fn run_takes_the_agent_and_its_arguments_without_a_separator() {
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
            "sh",
            "-c",
            "echo \"got $1 $2\"",
            "sh",
            "--intent",
            "--scope",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("got --intent --scope"));
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
    assert!(arg.contains("run `toimija enter` again"), "{arg}");
    assert!(arg.contains("## Session\n- id: "), "{arg}");
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
        .stderr(predicate::str::contains("expansions: none declared"));
}

/// `run` launches whatever state Git is in, and the packet says it plainly:
/// an operation stopped part-way and conflicted files are facts the agent
/// reads first, never a refusal toimija decides.
fn assert_run_launches_and_says(path: &Path, lines: &[&str]) {
    let output = Command::cargo_bin("toimija")
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
        .success();
    let packet = String::from_utf8_lossy(&output.get_output().stdout).into_owned();
    for line in lines {
        assert!(packet.contains(line), "no {line:?} in:\n{packet}");
    }
    assert!(path.join("CHILD_RAN").exists(), "the agent did not launch");
}

#[test]
fn run_launches_mid_merge_and_the_packet_says_so() {
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

    assert_run_launches_and_says(temp.path(), &["- operation: merge in progress\n"]);
}

#[test]
fn run_launches_with_conflicted_files_and_the_packet_says_so() {
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

    assert_run_launches_and_says(
        temp.path(),
        &[
            "- operation: merge in progress\n",
            "- conflicts: 1\n",
            "Conflicted files exist.",
        ],
    );
}

#[test]
fn run_launches_mid_rebase_and_the_packet_says_so() {
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

    assert_run_launches_and_says(temp.path(), &["- operation: rebase in progress\n"]);
}

/// The marker lives exactly as long as the session: the agent sees it while
/// it runs, and when it exits nothing is left — no marker, no packet file.
#[test]
fn run_leaves_no_marker_or_packet_behind() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let outside = tempfile::tempdir().unwrap();
    let during = outside.path().join("during.json");

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("DURING", &during)
        .args([
            "run",
            "--intent",
            "test",
            "--",
            "sh",
            "-c",
            "cat .toimija/sessions/*/session.json > \"$DURING\"",
        ])
        .assert()
        .success();

    let marker: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&during).unwrap()).unwrap();
    assert_eq!(marker["agent"][0], "sh", "{marker}");
    assert_eq!(marker["intent"], "test", "{marker}");
    assert!(marker["branch"].is_string(), "{marker}");
    let sessions = temp.path().join(".toimija/sessions");
    assert_eq!(fs::read_dir(&sessions).unwrap().count(), 0);
    assert!(!temp.path().join(".toimija/current.md").exists());
    assert!(!temp.path().join(".toimija/current.json").exists());
}

/// The launch packet names the session, and so does `toimija enter` inside
/// it, with what the session declared and where it started. Once the session
/// has ended, its id names nothing: the next agent never reads a dead
/// session's brief as its own.
#[test]
fn run_intent_reaches_the_packet_and_ends_with_the_session() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let outside = tempfile::tempdir().unwrap();
    let during = outside.path().join("during.json");
    let toimija = assert_cmd::cargo::cargo_bin("toimija");

    let output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("DURING", &during)
        .args([
            "run",
            "--intent",
            "test drive 2",
            "--scope",
            "src/a.rs",
            "--",
            "sh",
            "-c",
        ])
        .arg(format!(
            "'{}' enter --json > \"$DURING\" && printf '%s' \"$TOIMIJA_SESSION\" > id",
            toimija.display()
        ))
        .assert()
        .success();
    let launch = String::from_utf8_lossy(&output.get_output().stdout).into_owned();
    assert!(launch.contains("## Session\n- id: "), "{launch}");
    assert!(launch.contains("- intent: test drive 2\n"), "{launch}");
    assert!(launch.contains("- declared scope: src/a.rs\n"), "{launch}");

    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&during).unwrap()).unwrap();
    assert_eq!(json["session"]["intent"], "test drive 2");
    assert_eq!(json["session"]["scope"][0], "src/a.rs");
    assert_eq!(json["session"]["entry_branch"], json["git"]["branch"]);
    assert_eq!(json["session"]["entry_head"], json["git"]["head_oid"]);

    let id = fs::read_to_string(temp.path().join("id")).unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("TOIMIJA_SESSION", &id)
        .arg("enter")
        .assert()
        .success()
        .stdout(predicate::str::contains("## Session").not())
        .stdout(predicate::str::contains("test drive 2").not());
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
            "printf '%s\\n%s\\n%s\\n%s\\n%s' \"$TOIMIJA_RUN\" \"$TOIMIJA_SESSION\" \"$TOIMIJA_REPO_ROOT\" \"$TOIMIJA_RUN_ID\" \"${TOIMIJA_LIVE-unset}\" > env.txt",
        ])
        .assert()
        .success();

    let captured = fs::read_to_string(temp.path().join("env.txt")).unwrap();
    let mut lines = captured.lines();
    assert_eq!(lines.next(), Some("1"), "TOIMIJA_RUN should be 1");
    let session = lines.next().unwrap_or_default();
    assert!(!session.is_empty(), "TOIMIJA_SESSION should be set");
    let repo_root = std::fs::canonicalize(lines.next().unwrap()).unwrap();
    assert_eq!(repo_root, std::fs::canonicalize(temp.path()).unwrap());
    assert_eq!(lines.next(), Some(session), "TOIMIJA_RUN_ID is the session");
    // No packet file, so no variable that names one.
    assert_eq!(lines.next(), Some("unset"));
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
            "#!/bin/sh\nprintf '%s\\n' \"$TOIMIJA_SESSION\" \"$TOIMIJA_RUN_ID\" \"$@\" > captured-handshake\n",
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
        for (key, expected) in [("TOIMIJA_SESSION", lines[0]), ("TOIMIJA_RUN_ID", lines[1])] {
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
        let mut cleared: Vec<&str> = Vec::new();
        if !in_repo {
            cleared.push("TOIMIJA_REPO_ROOT");
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

/// A user who sets Codex's `developer_instructions` on the command line keeps
/// it. Toimija's own `-c` would come after it and replace it without a word,
/// so the packet goes in the first message instead, and `run` says why.
#[cfg(unix)]
#[test]
fn a_users_own_developer_instructions_survive_and_the_packet_goes_first() {
    use std::os::unix::fs::PermissionsExt;
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let stub = temp.path().join("codex");
    fs::write(
        &stub,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > captured-args\nfor last; do :; done\n\
         printf '%s' \"$last\" > launch-arg\n",
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--intent", "own slot", "--briefing", "Brief.", "--"])
        .arg(&stub)
        .args(["-c", "developer_instructions=\"MINE\""])
        .assert()
        .success()
        .stderr(predicate::str::contains("developer_instructions"));
    let captured = fs::read_to_string(temp.path().join("captured-args")).unwrap();
    let slots: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains("developer_instructions="))
        .collect();
    assert_eq!(slots, ["developer_instructions=\"MINE\""], "{captured}");
    let prompt = fs::read_to_string(temp.path().join("launch-arg")).unwrap();
    assert!(prompt.contains("# Toimija Context"), "{prompt}");
    assert!(prompt.ends_with("Brief.\n"), "{prompt}");
}

#[test]
fn run_ends_the_session_when_the_child_fails_to_launch() {
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
    assert_eq!(
        fs::read_dir(&sessions).unwrap().count(),
        0,
        "a failed launch must still remove the session marker"
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
        .success()
        // The plan is in the packet, to read, never to run.
        .stdout(predicate::str::contains("- touch VALIDATION_RAN"));

    assert!(
        !temp.path().join("VALIDATION_RAN").exists(),
        "run must never execute the configured validation plan"
    );
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

/// The agent command that saves the session's marker as the agent sees it.
const SAVE_MARKER: &str = "cat .toimija/sessions/*/session.json > marker.json";

fn marker_seen_by_the_agent(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path.join("marker.json")).unwrap()).unwrap()
}

#[test]
fn an_unknown_agent_is_told_its_briefing_was_not_delivered_and_the_session_records_it() {
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
            "--task",
            "t",
            "--briefing",
            "knowledge the agent needed",
            "--",
            "sh",
            "-c",
            SAVE_MARKER,
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "toimija run: warning: `sh` is not a known or configured agent; \
             not delivered: packet, task, briefing",
        ))
        .stderr(predicate::str::contains("agents:"));

    let marker = marker_seen_by_the_agent(temp.path());
    assert_eq!(
        marker["undelivered"],
        serde_json::json!(["packet", "task", "briefing"])
    );
}

#[test]
fn an_unknown_agent_without_task_or_briefing_still_hears_its_packet_was_not_delivered() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--intent", "test", "--", "sh", "-c", SAVE_MARKER])
        .assert()
        .success()
        .stderr(predicate::str::contains("not delivered: packet"));

    assert_eq!(
        marker_seen_by_the_agent(temp.path())["undelivered"],
        serde_json::json!(["packet"])
    );
}

/// A stub agent named `name` in `dir` that writes its last argument (the
/// composed prompt) to `launch-arg`.
#[cfg(unix)]
fn prompt_capturing_stub(dir: &Path, name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let stub = dir.join(name);
    fs::write(
        &stub,
        "#!/bin/sh\nfor last; do :; done\nprintf '%s' \"$last\" > launch-arg\n",
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    stub
}

const ALERT_HEADING: &str = "## Before anything else (from the launcher)";

#[cfg(unix)]
#[test]
fn alerts_open_the_prompt_above_the_packet_and_the_briefing_stays_last() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let stub = prompt_capturing_stub(temp.path(), "claude");

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "alerts",
            "--alert",
            "FLAGGED: main is red since 4f1c2a0",
            "--alert",
            "- WARNING: the last session left a dirty tree",
            "--alert",
            "   ",
            "--briefing",
            "Knowledge: long background text.",
            "--task",
            "fix it",
            "--",
        ])
        .arg(&stub)
        .assert()
        .success();

    let prompt = fs::read_to_string(temp.path().join("launch-arg")).unwrap();
    assert!(prompt.starts_with(ALERT_HEADING), "{prompt}");
    let alerts = prompt.find("FLAGGED: main is red since 4f1c2a0").unwrap();
    let warning = prompt
        .find("- WARNING: the last session left a dirty tree")
        .unwrap();
    let contract = prompt
        .find("Your toimija session packet is below.")
        .unwrap();
    let packet = prompt.find("# Toimija Context").unwrap();
    let task = prompt.find("Task: fix it").unwrap();
    let briefing = prompt.find("Knowledge: long background text.").unwrap();
    assert!(alerts < warning && warning < contract, "{prompt}");
    assert!(
        contract < packet && packet < task && task < briefing,
        "{prompt}"
    );
    // Each alert keeps its own text, one after the other; a blank one is none.
    let block: Vec<&str> = prompt[..contract]
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        block,
        [
            "FLAGGED: main is red since 4f1c2a0",
            "- WARNING: the last session left a dirty tree"
        ],
        "{prompt}"
    );
}

#[cfg(unix)]
#[test]
fn alerts_reach_idle_codex_and_sessions_outside_a_repository() {
    if !git_available() {
        return;
    }
    // Idle Codex: the alerts open its session instructions.
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let stub = temp.path().join("codex");
    fs::write(&stub, "#!/bin/sh\nprintf '%s\\n' \"$@\" > captured-args\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["run", "--intent", "idle", "--alert", "FLAGGED: x", "--"])
        .arg(&stub)
        .assert()
        .success();
    let captured = fs::read_to_string(temp.path().join("captured-args")).unwrap();
    let encoded = captured
        .lines()
        .find_map(|line| line.strip_prefix("developer_instructions="))
        .unwrap();
    let instructions = serde_json::from_str::<String>(encoded).unwrap();
    assert!(instructions.starts_with(ALERT_HEADING), "{instructions}");
    assert!(instructions.contains("FLAGGED: x"), "{instructions}");

    // Outside any repository: the alerts open the no-packet prompt.
    let folder = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let stub = prompt_capturing_stub(folder.path(), "claude");
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(folder.path())
        .env_remove("TOIMIJA_SESSION")
        .env("TOIMIJA_STATE_DIR", state.path())
        .env("GIT_CEILING_DIRECTORIES", folder.path().parent().unwrap())
        .args(["run", "--intent", "outside", "--alert", "FLAGGED: y", "--"])
        .arg(&stub)
        .assert()
        .success();
    let prompt = fs::read_to_string(folder.path().join("launch-arg")).unwrap();
    assert!(prompt.starts_with(ALERT_HEADING), "{prompt}");
    assert!(prompt.contains("outside any Git repository"), "{prompt}");
}

#[cfg(unix)]
#[test]
fn alerts_are_capped_near_a_kilobyte_and_an_unknown_agent_is_told_it_missed_them() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let stub = prompt_capturing_stub(temp.path(), "claude");
    let long = "é".repeat(400); // 800 bytes, two-byte characters
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run", "--intent", "cap", "--alert", &long, "--alert", &long, "--",
        ])
        .arg(&stub)
        .assert()
        .success()
        .stderr(predicate::str::contains("alerts exceed 1024 bytes"));
    let prompt = fs::read_to_string(temp.path().join("launch-arg")).unwrap();
    let block = &prompt[..prompt
        .find("Your toimija session packet is below.")
        .unwrap()];
    assert!(block.matches('é').count() * 2 <= 1024, "{block}");
    assert!(block.contains("truncated"), "{block}");

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "unknown",
            "--alert",
            "FLAGGED: z",
            "--",
            "sh",
            "-c",
            "true",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("not delivered: alert, packet"));
}

/// Ohjaa launches every session as `toimija run --intent <why> --task <what>
/// --briefing <knowledge> --scope <path>... -- <agent>`, then finds that
/// session with `toimija sessions --json`: by an id of the shape
/// `<secs>-<pid>`, the pid being `toimija run`'s own, and reads its agent,
/// worktree, branch, intent, scope, liveness, pid_check and last_seen_at, and
/// the report's warnings and coverage. All of that, together, as ohjaa asks.
#[cfg(unix)]
#[test]
fn run_takes_the_flags_ohjaa_launches_with_and_sessions_reports_what_it_reads() {
    use std::os::unix::fs::PermissionsExt;
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let toimija = assert_cmd::cargo::cargo_bin("toimija");
    let stub = temp.path().join("claude");
    fs::write(
        &stub,
        format!(
            "#!/bin/sh\nfor last; do :; done\nprintf '%s' \"$last\" > launch-arg\n\
             printf '%s' \"$PPID\" > ppid\n'{}' sessions --json > during.json\n",
            toimija.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("TOIMIJA_SESSION")
        .args([
            "run",
            "--intent",
            "ohjaa intent",
            "--task",
            "the assigned task",
            "--briefing",
            "Knowledge: what ohjaa knows.",
            "--scope",
            "src/a.rs",
            "--scope",
            "docs/",
            "--",
        ])
        .arg(&stub)
        .assert()
        .success();

    let prompt = fs::read_to_string(temp.path().join("launch-arg")).unwrap();
    let task = prompt.find("Task: the assigned task").unwrap();
    // The instruction above the packet names sections the packet has.
    assert!(prompt.contains("Read first list"), "{prompt}");
    assert!(prompt.contains("\n## Read first\n"), "{prompt}");
    let briefing = prompt.find("Knowledge: what ohjaa knows.").unwrap();
    assert!(task < briefing, "{prompt}");

    let during: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(temp.path().join("during.json")).unwrap())
            .unwrap();
    assert_eq!(during["coverage"]["status"], "available", "{during}");
    assert!(during["warnings"].is_array(), "{during}");
    let sessions = during["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{during}");
    let session = &sessions[0];
    let ppid = fs::read_to_string(temp.path().join("ppid")).unwrap();
    let id = session["id"].as_str().unwrap();
    let (secs, pid) = id.split_once('-').unwrap();
    assert!(secs.parse::<u64>().is_ok(), "{id}");
    assert_eq!(pid, ppid.trim(), "the id names `toimija run`'s pid");
    assert_eq!(session["agent"][0], stub.display().to_string());
    assert!(session["worktree"].is_string(), "{session}");
    assert!(session["branch"].is_string(), "{session}");
    assert_eq!(session["intent"], "ohjaa intent");
    assert_eq!(session["scope"], serde_json::json!(["src/a.rs", "docs/"]));
    assert_eq!(session["liveness"], "active");
    assert_eq!(session["pid_check"], "alive");
    assert!(session["last_seen_at"].as_u64().is_some(), "{session}");

    let after = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["sessions", "--json"])
        .assert()
        .success();
    let after: serde_json::Value = serde_json::from_slice(&after.get_output().stdout).unwrap();
    // The session ended with its agent: it is no longer listed.
    assert_eq!(after["sessions"], serde_json::json!([]), "{after}");
}

/// `--task`, `--briefing` and `--alert` take text that may start
/// with `-`, so one whose value was left out could take the next flag as
/// its text: `--briefing --task "fix it"` would launch a program named `fix it`.
/// A value that is one of `run`'s own flags (or the `--` separator) is a
/// missing value: refused before any session opens, and nothing launches.
#[test]
fn a_flag_where_a_value_belongs_is_a_missing_value_and_nothing_launches() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let launch = ["--", "sh", "-c", "touch CHILD_RAN"];
    for (flag, given) in [
        (
            &["--briefing", "--task", "fix it"][..],
            "`--task`, one of `run`'s own flags,",
        ),
        (
            &["--task", "--briefing", "background"],
            "`--briefing`, one of `run`'s own flags,",
        ),
        (&["--alert", "--"], "the `--` before the command"),
        (
            &["--briefing", "--scope=src/"],
            "`--scope=src/`, one of `run`'s own flags,",
        ),
        (
            &["--task", "--for-diff"],
            "`--for-diff`, one of `run`'s own flags,",
        ),
        (&["--briefing", "-q"], "`-q`, one of `run`'s own flags,"),
    ] {
        let missing = flag[0];
        Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(temp.path())
            .args(["run", "--intent", "missing values"])
            .args(flag)
            .args(launch)
            .assert()
            .code(2)
            .stderr(predicate::str::contains(format!(
                "`{missing}` is missing its value: {given} is not text"
            )));
        assert!(!temp.path().join("CHILD_RAN").exists(), "{flag:?}");
        assert!(!temp.path().join(".toimija/sessions").exists(), "{flag:?}");
    }

    // Text that only starts with a hyphen is still text.
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "hyphenated text",
            "--task",
            "--- read the notes, then fix it",
            "--briefing",
            "- check the receipt first",
            "--alert",
            "-- FLAGGED: main is red",
        ])
        .args(launch)
        .assert()
        .success();
    assert!(temp.path().join("CHILD_RAN").exists());
}

/// The session summary counts the work the session leaves behind, never
/// Toimija's own files: where the repository does not ignore them, the
/// handover is visible to Git, and must not count as untracked work.
#[test]
fn the_exit_summary_does_not_count_toimijas_own_files() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["handover", "--next", "visible to git"])
        .assert()
        .success();
    for _ in 0..2 {
        Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(temp.path())
            .args([
                "run",
                "--intent",
                "count",
                "--",
                "sh",
                "-c",
                "touch work.txt",
            ])
            .assert()
            .success()
            .stderr(predicate::str::contains(
                "worktree:  0 staged, 0 unstaged, 1 untracked\n",
            ));
    }
}

/// The `=` form says the value is intended: `--task=--briefing` passes the
/// text `--briefing`, which the spaced form refuses as a missing value. So a
/// task, briefing or alert that is literally one of `run`'s flags, or `--`,
/// can still be given.
#[cfg(unix)]
#[test]
fn the_equals_form_passes_any_text_even_a_flags_name() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let stub = prompt_capturing_stub(temp.path(), "claude");
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "literal flags",
            "--task=--briefing",
            "--briefing=--task",
            "--alert=--",
            "--",
        ])
        .arg(&stub)
        .assert()
        .success();
    let prompt = fs::read_to_string(temp.path().join("launch-arg")).unwrap();
    assert!(prompt.contains("Task: --briefing"), "{prompt}");
    assert!(prompt.trim_end().ends_with("--task"), "{prompt}");
    let alerts = prompt
        .split("Your toimija session packet is below.")
        .next()
        .unwrap();
    assert!(alerts.lines().any(|line| line == "--"), "{prompt}");
}

/// A stub agent named `name` in `dir` that writes every argument it gets,
/// each ended by a NUL, to `launch-args`.
#[cfg(unix)]
fn argv_capturing_stub(dir: &Path, name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let stub = dir.join(name);
    fs::write(
        &stub,
        "#!/bin/sh\nfor arg; do printf '%s\\0' \"$arg\"; done > launch-args\n",
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    stub
}

/// pi and opencode get the packet and the briefing in their launch prompt,
/// each the way it takes an initial prompt: pi as a message argument,
/// opencode after `--prompt` (its one positional argument is the folder to
/// start in). Under env-var delivery alone neither ever saw the briefing,
/// and none of their 13 sessions launched through ohjaa read it back.
#[cfg(unix)]
#[test]
fn pi_and_opencode_get_the_packet_in_their_launch_prompt() {
    if !git_available() {
        return;
    }
    for (name, flag) in [("pi", None), ("opencode", Some("--prompt"))] {
        let temp = tempfile::tempdir().unwrap();
        init_git_repo(temp.path());
        let stub = argv_capturing_stub(temp.path(), name);
        Command::cargo_bin("toimija")
            .unwrap()
            .current_dir(temp.path())
            .args([
                "run",
                "--intent",
                "delivery",
                "--briefing",
                "Knowledge: read it back.",
                "--",
            ])
            .arg(&stub)
            .assert()
            .success()
            .stderr(predicate::str::contains("env-var only").not());
        let args = fs::read_to_string(temp.path().join("launch-args")).unwrap();
        let args: Vec<&str> = args.split_terminator('\0').collect();
        let prompt = match flag {
            None => {
                assert_eq!(args.len(), 1, "{name}: {args:?}");
                args[0]
            }
            Some(flag) => {
                assert_eq!(args.len(), 2, "{name}: {args:?}");
                assert_eq!(args[0], flag, "{name}: {args:?}");
                args[1]
            }
        };
        assert!(
            prompt.contains("Your toimija session packet is below.")
                && prompt.contains("# Toimija Context")
                && prompt.contains("Knowledge: read it back."),
            "{name}: {prompt}"
        );
    }
}

/// `opencode run` takes its message as arguments and has no `--prompt`
/// (opencode 2.2.5), so the packet goes after the person's own arguments as
/// the message.
#[cfg(unix)]
#[test]
fn opencode_run_gets_the_packet_as_its_message() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let stub = argv_capturing_stub(temp.path(), "opencode");
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "run",
            "--intent",
            "delivery",
            "--task",
            "fix the build",
            "--",
        ])
        .arg(&stub)
        .args(["run", "--model", "a/b"])
        .assert()
        .success();
    let args = fs::read_to_string(temp.path().join("launch-args")).unwrap();
    let args: Vec<&str> = args.split_terminator('\0').collect();
    assert_eq!(args.len(), 4, "{args:?}");
    assert_eq!(&args[..3], ["run", "--model", "a/b"], "{args:?}");
    assert!(
        args[3].contains("Your toimija session packet is below.")
            && args[3].contains("Task: fix the build"),
        "{}",
        args[3]
    );
}
