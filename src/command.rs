use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use tracing::{debug, info};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait CommandRunner {
    fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput>;

    fn run_with_env(
        &self,
        cwd: &Path,
        program: &str,
        args: &[&str],
        env: &[(&str, &OsStr)],
    ) -> std::io::Result<CommandOutput> {
        if env.is_empty() {
            self.run(cwd, program, args)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "command runner does not support environment assignments",
            ))
        }
    }
}

/// Git resolves the repository from these environment variables *before* it
/// consults the process working directory, so an inherited value (leaked from a
/// git hook, a wrapping process, or an exported shell var) silently overrides
/// the `cwd` we pin — Toimija would then compile a packet for the wrong worktree
/// (wrong branch, wrong index, even a phantom in-progress merge). We strip them
/// so `cwd` is the sole source of truth. Identity/auth/config vars
/// (`GIT_AUTHOR_*`, `GIT_SSH`, `GIT_CONFIG*`) are left alone — they don't
/// relocate the repo, and callers may legitimately rely on them.
const GIT_LOCATION_ENV: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// Remove the repo-locating git env vars from `command` so repository discovery
/// falls back to the working directory we set. See [`GIT_LOCATION_ENV`].
fn scrub_git_location_env(command: &mut Command) {
    for key in GIT_LOCATION_ENV {
        command.env_remove(key);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        self.run_with_env(cwd, program, args, &[])
    }

    fn run_with_env(
        &self,
        cwd: &Path,
        program: &str,
        args: &[&str],
        env: &[(&str, &OsStr)],
    ) -> std::io::Result<CommandOutput> {
        let started = Instant::now();
        debug!(
            event = "toimija.git.command.start",
            command = program,
            args = ?args,
            cwd = %cwd.display()
        );
        let mut command = Command::new(program);
        command.args(args).current_dir(cwd);
        scrub_git_location_env(&mut command);
        command.envs(env.iter().copied());
        let output = command.output()?;
        let status = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        debug!(
            event = "toimija.git.command.finish",
            command = program,
            args = ?args,
            cwd = %cwd.display(),
            exit_status = status,
            duration_ms = started.elapsed().as_millis() as u64,
            stdout_bytes = output.stdout.len(),
            stderr_bytes = output.stderr.len()
        );
        Ok(CommandOutput {
            status,
            stdout,
            stderr,
        })
    }
}

pub fn command_available(program: &str) -> bool {
    let available = which::which(program).is_ok();
    info!(
        event = "toimija.command.available",
        command = program,
        available
    );
    available
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsStr;

    #[test]
    fn scrub_marks_every_repo_location_var_for_removal() {
        // `env_remove` records the key with a `None` value in the command's env
        // mutations, which `get_envs` reports without touching the real process
        // environment — so this stays hermetic and race-free under parallel tests.
        let mut command = Command::new("git");
        scrub_git_location_env(&mut command);

        let mutations: HashMap<&OsStr, Option<&OsStr>> = command.get_envs().collect();
        for key in GIT_LOCATION_ENV {
            assert_eq!(
                mutations.get(OsStr::new(key)),
                Some(&None),
                "{key} should be removed so cwd wins repository discovery"
            );
        }
    }

    #[test]
    fn leaked_git_dir_does_not_redirect_discovery() {
        // End-to-end: with GIT_DIR pointed at repo B, the runner must still
        // resolve the repo from `cwd` (repo A). This is the exact phantom-merge
        // failure mode. Serialized via a lock because it mutates process env.
        use std::sync::Mutex;
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap();

        let temp = tempfile::tempdir().unwrap();
        let repo_a = temp.path().join("a");
        let repo_b = temp.path().join("b");
        let runner = ProcessCommandRunner;
        for repo in [&repo_a, &repo_b] {
            std::fs::create_dir_all(repo).unwrap();
            runner.run(repo, "git", &["init", "-q"]).unwrap();
        }

        // SAFETY: guarded by ENV_LOCK; removed before the guard drops.
        unsafe { std::env::set_var("GIT_DIR", repo_b.join(".git")) };
        let toplevel = runner
            .run(&repo_a, "git", &["rev-parse", "--show-toplevel"])
            .unwrap();
        unsafe { std::env::remove_var("GIT_DIR") };

        // Resolve symlinks (macOS /var -> /private/var) before comparing.
        let seen = std::fs::canonicalize(toplevel.stdout.trim()).unwrap();
        let want = std::fs::canonicalize(&repo_a).unwrap();
        assert_eq!(seen, want, "leaked GIT_DIR must not override cwd");
    }
}
