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
}

#[derive(Debug, Clone, Copy)]
pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        let started = Instant::now();
        debug!(
            event = "toimija.git.command.start",
            command = program,
            args = ?args,
            cwd = %cwd.display()
        );
        let output = Command::new(program).args(args).current_dir(cwd).output()?;
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
