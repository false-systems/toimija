use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use tracing::{debug, info};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandOutput {
    pub fn stdout_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }

    pub fn stderr_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.stderr)
    }
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

/// A `git` pinned to `cwd`, for the few callers that need a [`Command`] of
/// their own (stdin, a scratch `GIT_INDEX_FILE`) rather than the runner. The
/// repo-locating variables it would inherit are removed, as the runner removes
/// them; a caller sets what it means to on the command, never process-wide.
pub fn git_command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(cwd);
    scrub_git_location_env(&mut command);
    command
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
            stdout: output.stdout,
            stderr: output.stderr,
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

    /// The commands built outside the runner are scrubbed the same way. That
    /// a child git started by toimija with `GIT_DIR` and friends inherited
    /// still acts on its own directory is shown end to end, with the
    /// variables set on toimija's own command only, by
    /// `tests/git_isolation.rs`: setting them process-wide here, even for a
    /// moment under a lock, reached every fixture git running beside it.
    #[test]
    fn a_git_command_is_pinned_to_its_directory() {
        let command = git_command(Path::new("/repo"));
        assert_eq!(command.get_current_dir(), Some(Path::new("/repo")));
        let mutations: HashMap<&OsStr, Option<&OsStr>> = command.get_envs().collect();
        for key in GIT_LOCATION_ENV {
            assert_eq!(mutations.get(OsStr::new(key)), Some(&None), "{key}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn runner_preserves_invalid_utf8_until_text_is_requested() {
        let output = ProcessCommandRunner
            .run(Path::new("."), "sh", &["-c", "printf '\\377'"])
            .unwrap();

        assert_eq!(output.stdout, vec![0xff]);
        assert_eq!(output.stdout_lossy(), "�");
    }
}
