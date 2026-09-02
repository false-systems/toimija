use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToimijaError {
    #[error("not a Git repository: run `git init` or move into a repo")]
    NotGitRepo,
    #[error("missing Toimija config: run `toimija init`")]
    MissingConfig,
    #[error("invalid repo config at {path}: {source}")]
    InvalidConfig {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    #[error("invalid stewardship config at {path}: {message}")]
    InvalidStewardship { path: PathBuf, message: String },
    #[error("invalid agents config at {path}: {message}")]
    InvalidAgents { path: PathBuf, message: String },
    #[error("failed to read config at {path}: {source}")]
    ReadConfig {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("git command failed: {command} exited {status}")]
    GitCommand { command: String, status: i32 },
    #[error("required command not found: {0}")]
    CommandNotFound(String),
}

pub type Result<T> = std::result::Result<T, ToimijaError>;
