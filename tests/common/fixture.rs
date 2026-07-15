//! Reusable hermetic Git fixture builder for release-QA scenarios.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::{TempDir, tempdir};

use super::git_at;

pub struct RepoFixture {
    pub repo: TempDir,
    home: TempDir,
}

impl RepoFixture {
    pub fn new() -> Self {
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let fixture = Self { repo, home };
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.email", "qa@example.invalid"]);
        fixture.git(&["config", "user.name", "Toimija QA"]);
        fixture.git(&["config", "commit.gpgSign", "false"]);
        fixture.git(&["config", "tag.gpgSign", "false"]);
        fixture
    }

    pub fn path(&self) -> &Path {
        self.repo.path()
    }

    pub fn write(&self, relative: impl AsRef<Path>, contents: impl AsRef<[u8]>) {
        let path = self.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    pub fn stage(&self, paths: &[&str]) {
        let mut args = vec!["add", "--"];
        args.extend(paths);
        self.git(&args);
    }

    pub fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-m", message]);
    }

    pub fn branch(&self, name: &str) {
        self.git(&["switch", "-c", name]);
    }

    pub fn switch(&self, name: &str) {
        self.git(&["switch", name]);
    }

    pub fn git(&self, args: &[&str]) -> Output {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    pub fn git_expect_failure(&self, args: &[&str]) -> Output {
        let output = self.command().args(args).output().unwrap();
        assert!(
            !output.status.success(),
            "git {args:?} unexpectedly succeeded"
        );
        output
    }

    pub fn command(&self) -> Command {
        let mut command = git_at(self.path());
        command
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("GIT_CONFIG_NOSYSTEM", "1");
        command
    }

    pub fn git_dir(&self) -> PathBuf {
        self.path().join(".git")
    }
}
