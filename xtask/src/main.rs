//! Toimija developer tasks.
//!
//! This binary doubles as the git `pre-commit` hook (via `devx-pre-commit`).
//! When git invokes it, `argv[0]` ends with `pre-commit`, so it runs the hook
//! logic; otherwise it acts as a small dev CLI.
//!
//! Install the hook with:
//!
//! ```text
//! cargo xtask install-pre-commit-hook
//! ```

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{collections::HashSet, collections::VecDeque};

use anyhow::{bail, Result};
use devx_pre_commit::{install_self_as_hook, locate_project_root, PreCommitContext};
use serde::Deserialize;

fn run_hook() -> Result<()> {
    let mut ctx = PreCommitContext::from_git_diff(locate_project_root()?)?;
    // Never touch generated code, only hand-written sources.
    ctx.retain_staged_files(|path| {
        path.components()
            .all(|component| component.as_os_str() != OsStr::new("generated"))
    });
    // Format the staged Rust crates and re-stage the results so the commit
    // records the formatted code rather than rejecting it.
    ctx.rustfmt()?;
    ctx.stage_new_changes()?;
    // Lint gate: a commit must not introduce clippy warnings. This mirrors the
    // CI step (`cargo clippy --all-targets -- -D warnings`) so failures surface
    // locally instead of after a push.
    run_clippy()?;
    // Test gate: the suite must be green before the commit lands. Runs after
    // clippy so the cheaper lint failures surface first.
    run_tests()?;
    Ok(())
}

fn run_clippy() -> Result<()> {
    run_gate(
        &["clippy", "--all-targets", "--", "-D", "warnings"],
        "clippy reported issues",
    )
}

fn run_tests() -> Result<()> {
    run_gate(&["test"], "tests failed")
}

/// Git environment variables that redirect git at a specific repo, index, or
/// worktree. Git exports these when it invokes a hook; if they leak into the
/// gates' `cargo` process, the integration tests' `git` calls and the
/// `toimija`-under-test both act on the real repo instead of their tempdirs —
/// once observed flipping `core.bare = true` and bricking a sibling worktree.
const REDIRECTING_GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_CEILING_DIRECTORIES",
];

/// Run a `cargo` subcommand and abort the commit if it exits non-zero. The git
/// redirection env is scrubbed so the gates run hermetically against tempdirs.
fn run_gate(args: &[&str], what: &str) -> Result<()> {
    let mut command = Command::new(cargo());
    command.args(args);
    for var in REDIRECTING_GIT_ENV {
        command.env_remove(var);
    }
    let status = command.status()?;
    if !status.success() {
        bail!("{what}; commit aborted. Fix the problem and commit again.");
    }
    Ok(())
}

/// Honour the `CARGO` env var git/cargo sets, falling back to `cargo` on PATH.
fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

#[derive(Debug)]
struct Artifact {
    schema: String,
    digest: String,
    producer: String,
    run: Option<String>,
    anchored_at: String,
    verified: bool,
    edges: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    schema: String,
    digest: String,
    provenance: Vec<Provenance>,
    producer: Producer,
    anchored_at: String,
}

#[derive(Debug, Deserialize)]
struct Producer {
    tool: String,
    run: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Provenance {
    Git { oid: String },
    Token { token: String },
    Artifact { digest: String },
}

fn run_sauma_why(query: &str) -> Result<()> {
    let root = locate_project_root()?;
    let artifacts = scan_artifacts(&root)?;
    let mut queue: VecDeque<usize> = artifacts
        .iter()
        .enumerate()
        .filter_map(|(index, artifact)| touches(artifact, query).then_some(index))
        .collect();
    let mut seen = HashSet::new();
    while let Some(index) = queue.pop_front() {
        if !seen.insert(index) {
            continue;
        }
        let artifact = &artifacts[index];
        println!(
            "{} {} ({}, {}) ← {}{}",
            artifact.schema,
            short(&artifact.digest),
            artifact.producer,
            short(&artifact.anchored_at),
            artifact
                .edges
                .first()
                .map(|edge| short(edge))
                .unwrap_or_else(|| "root".to_string()),
            if artifact.verified {
                ""
            } else {
                " [digest mismatch]"
            }
        );
        for edge in &artifact.edges {
            for (next, candidate) in artifacts.iter().enumerate() {
                if candidate.digest == *edge || candidate.anchored_at == *edge {
                    queue.push_back(next);
                }
            }
        }
    }
    if seen.is_empty() {
        println!("no sauma chain touches {query}");
    }
    Ok(())
}

fn touches(artifact: &Artifact, query: &str) -> bool {
    artifact.digest == query
        || artifact.anchored_at == query
        || artifact.producer == query
        || artifact.run.as_deref() == Some(query)
        || artifact.edges.iter().any(|edge| edge == query)
}

fn scan_artifacts(root: &Path) -> Result<Vec<Artifact>> {
    let mut files = Vec::new();
    push_if_exists(&mut files, root.join(".toimija/current.json"));
    let sessions = root.join(".toimija/sessions");
    if let Ok(entries) = fs::read_dir(&sessions) {
        for entry in entries.flatten() {
            let dir = entry.path();
            push_if_exists(&mut files, dir.join("current.json"));
            push_if_exists(&mut files, dir.join("handoff.jsonl"));
        }
    }
    let runs = root.join(".toimija/runs");
    if let Ok(entries) = fs::read_dir(&runs) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("jsonl")) {
                files.push(path);
            }
        }
    }
    let mut artifacts = Vec::new();
    for path in files {
        let raw = fs::read_to_string(&path)?;
        if path.extension() == Some(OsStr::new("jsonl")) {
            for line in raw.lines() {
                collect_artifact(line, &mut artifacts)?;
            }
            collect_artifact(&raw, &mut artifacts)?;
        } else {
            collect_artifact(&raw, &mut artifacts)?;
        }
    }
    Ok(artifacts)
}

fn push_if_exists(files: &mut Vec<PathBuf>, path: PathBuf) {
    if path.exists() {
        files.push(path);
    }
}

fn collect_artifact(raw: &str, artifacts: &mut Vec<Artifact>) -> Result<()> {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(());
    };
    let Some(sauma_value) = value.get("sauma").cloned() else {
        return Ok(());
    };
    let envelope: Envelope = serde_json::from_value(sauma_value)?;
    if artifacts
        .iter()
        .any(|artifact| artifact.digest == envelope.digest)
    {
        return Ok(());
    }
    if let serde_json::Value::Object(object) = &mut value {
        object.swap_remove("sauma");
    }
    let canonical = serde_json::to_string(&value)?;
    let actual = toimija::sauma::digest_json(&canonical);
    let mut edges: Vec<String> = envelope
        .provenance
        .into_iter()
        .map(|edge| match edge {
            Provenance::Git { oid } => oid,
            Provenance::Token { token } => token,
            Provenance::Artifact { digest } => digest,
        })
        .collect();
    edges.push(envelope.anchored_at.clone());
    let run = envelope.producer.run;
    let producer = run
        .as_ref()
        .map(|run| format!("{}/{}", envelope.producer.tool, run))
        .unwrap_or(envelope.producer.tool);
    artifacts.push(Artifact {
        schema: envelope.schema,
        digest: envelope.digest.clone(),
        producer,
        run,
        anchored_at: envelope.anchored_at,
        verified: actual == envelope.digest,
        edges,
    });
    Ok(())
}

fn short(value: &str) -> String {
    value
        .strip_prefix("sha256:")
        .unwrap_or(value)
        .chars()
        .take(12)
        .collect()
}

fn main() -> Result<()> {
    // Git runs the hook as `.git/hooks/pre-commit`, so argv[0] contains it.
    if std::env::args()
        .next()
        .is_some_and(|arg0| arg0.contains("pre-commit"))
    {
        return run_hook();
    }
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("install-pre-commit-hook") => {
            install_self_as_hook(&locate_project_root()?)?;
            eprintln!("Installed git pre-commit hook (.git/hooks/pre-commit).");
            eprintln!(
                "It runs rustfmt on staged crates, re-stages, then clippy -D warnings and tests."
            );
        }
        Some("sauma-why") => {
            let Some(query) = args.get(2) else {
                bail!("usage: cargo xtask sauma-why <digest|token|session-id>");
            };
            run_sauma_why(query)?;
        }
        other => {
            if let Some(arg) = other {
                eprintln!("Unknown task: {arg}\n");
            }
            eprintln!("toimija dev tasks:");
            eprintln!("  cargo xtask install-pre-commit-hook   install the git pre-commit hook");
            eprintln!("  cargo xtask sauma-why <query>        walk local sauma artifact edges");
        }
    }
    Ok(())
}
