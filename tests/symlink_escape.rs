//! Nothing Toimija writes, appends, or deletes may follow a symlink out of the
//! repository.
//!
//! A repository can commit `.toimija`, any directory under it, or any file
//! Toimija writes there, as a symlink. Each case below points one at a
//! directory outside the repository, runs the commands that write through
//! that path, and checks two things: the command refuses loudly, and the
//! outside directory is byte-for-byte what it was.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;

mod common;
use common::{git_available, run_git};

struct Fixture {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    outside: PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&outside).unwrap();
    run_git(&repo, &["init"]);
    run_git(&repo, &["config", "user.email", "toimija@example.invalid"]);
    run_git(&repo, &["config", "user.name", "Toimija Test"]);
    fs::write(repo.join("README.md"), "# Demo\n").unwrap();
    run_git(&repo, &["add", "README.md"]);
    run_git(&repo, &["commit", "-m", "initial"]);
    fs::write(outside.join("sentinel.txt"), "outside the repository\n").unwrap();
    Fixture {
        _temp: temp,
        repo,
        outside,
    }
}

impl Fixture {
    /// Make `relative` (inside the repo) a symlink to `target` (inside the
    /// outside directory), creating real parent directories as needed.
    fn link(&self, relative: &str, target: &str) {
        let path = self.repo.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(self.outside.join(target), path).unwrap();
    }

    fn toimija(&self) -> Command {
        let mut command = Command::cargo_bin("toimija").unwrap();
        command
            .current_dir(&self.repo)
            .env_remove("TOIMIJA_SESSION")
            .env_remove("TOIMIJA_RUN");
        command
    }

    /// Every file under the outside directory with its bytes.
    fn outside_state(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut state = BTreeMap::new();
        let mut pending = vec![self.outside.clone()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                if meta.is_dir() {
                    pending.push(path);
                } else {
                    state.insert(
                        path.strip_prefix(&self.outside).unwrap().to_path_buf(),
                        fs::read(&path).unwrap_or_default(),
                    );
                }
            }
        }
        state
    }

    /// Run `args`, expect a loud refusal naming a symlink, and expect the
    /// outside directory untouched.
    fn assert_refused(&self, args: &[&str], envs: &[(&str, &str)]) {
        let before = self.outside_state();
        let mut command = self.toimija();
        for (key, value) in envs {
            command.env(key, value);
        }
        command
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("symlink"));
        assert_eq!(
            self.outside_state(),
            before,
            "`toimija {}` changed files outside the repository",
            args.join(" ")
        );
    }
}

/// A marker for session `id`, held by this test process. `pid_start` set to a
/// start this process never had makes the session provably ended.
fn marker(id: &str, ended: bool) -> String {
    let mut marker = serde_json::json!({
        "id": id,
        "pid": std::process::id(),
        "agent": ["claude"],
        "worktree": "/repo",
        "branch": "main",
        "started_at": 100,
    });
    if ended {
        marker["pid_start"] = serde_json::json!(1);
    }
    marker.to_string()
}

#[test]
fn a_symlinked_toimija_dir_is_refused_by_every_writer() {
    if !git_available() {
        return;
    }
    let fx = fixture();
    fx.link(".toimija", "");

    for args in [
        &["run", "--intent", "test", "--", "true"][..],
        &["handover", "--next", "hijacked"],
    ] {
        fx.assert_refused(args, &[]);
    }
}

#[test]
fn a_symlinked_sessions_dir_is_refused_by_run() {
    if !git_available() {
        return;
    }
    let fx = fixture();
    fx.link(".toimija/sessions", "");
    fx.assert_refused(&["run", "--intent", "test", "--", "true"], &[]);
}

#[test]
fn a_symlinked_session_dir_is_refused_by_intent() {
    if !git_available() {
        return;
    }
    let fx = fixture();
    fs::create_dir_all(fx.outside.join("session")).unwrap();
    fs::write(
        fx.outside.join("session/session.json"),
        marker("100-1", false),
    )
    .unwrap();
    fx.link(".toimija/sessions/100-1", "session");

    fx.assert_refused(&["intent", "hijacked"], &[("TOIMIJA_SESSION", "100-1")]);
}

/// `run` removes the markers of sessions that provably ended beside it. One
/// behind a committed symlink is never removed through it: `run` carries on,
/// and the outside directory is untouched.
#[test]
fn an_ended_session_behind_a_symlink_is_never_removed_through_it() {
    if !git_available() {
        return;
    }
    let fx = fixture();
    fs::create_dir_all(fx.outside.join("session")).unwrap();
    fs::write(
        fx.outside.join("session/session.json"),
        marker("100-1", true),
    )
    .unwrap();
    fx.link(".toimija/sessions/100-1", "session");
    let before = fx.outside_state();

    fx.toimija()
        .args(["run", "--intent", "test", "--", "true"])
        .assert()
        .success();
    assert_eq!(fx.outside_state(), before);
    assert!(fx.outside.join("session/session.json").exists());
}
