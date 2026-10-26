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

fn marker(id: &str, status: &str) -> String {
    serde_json::json!({
        "id": id,
        "pid": 1,
        "agent": ["claude"],
        "worktree": "/repo",
        "branch": "main",
        "started_at": 100,
        "status": status,
        "closed_at": if status == "closed" { Some(100) } else { None },
    })
    .to_string()
}

#[test]
fn a_symlinked_toimija_dir_is_refused_by_every_writer() {
    if !git_available() {
        return;
    }
    let fx = fixture();
    fx.link(".toimija", "");

    for args in [
        &["enter"][..],
        &["refresh"],
        &["init"],
        &["clean"],
        &["snapshot", "--lease"],
        &["run", "--intent", "test", "--", "true"],
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
fn a_symlinked_session_dir_is_refused_by_the_session_writers() {
    if !git_available() {
        return;
    }
    let fx = fixture();
    fs::create_dir_all(fx.outside.join("session")).unwrap();
    fs::write(
        fx.outside.join("session/session.json"),
        marker("100-1", "active"),
    )
    .unwrap();
    fx.link(".toimija/sessions/100-1", "session");

    let session = [("TOIMIJA_SESSION", "100-1")];
    fx.assert_refused(&["intent", "hijacked"], &session);
    fx.assert_refused(&["enter"], &session);
    fx.assert_refused(&["refresh"], &session);
}

#[test]
fn a_symlinked_history_journal_or_lock_is_never_appended_to() {
    if !git_available() {
        return;
    }
    for link in [".toimija/history.jsonl", ".toimija/history.lock"] {
        let fx = fixture();
        // A closed session long past retention, so `clean` journals it.
        let dir = fx.repo.join(".toimija/sessions/100-1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("session.json"), marker("100-1", "closed")).unwrap();
        fx.link(link, "sentinel.txt");

        fx.assert_refused(&["clean"], &[]);
    }
}

#[test]
fn a_symlinked_snapshots_dir_is_refused_by_lease_and_release() {
    if !git_available() {
        return;
    }
    let fx = fixture();
    fs::write(fx.outside.join("victim.json"), "{}\n").unwrap();
    fx.link(".toimija/snapshots", "");

    fx.assert_refused(&["snapshot", "--lease"], &[]);
    fx.assert_refused(&["snapshot", "--release", "victim"], &[]);
}
