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

use anyhow::{bail, Result};
use devx_pre_commit::{install_self_as_hook, locate_project_root, PreCommitContext};

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
    anchored_at: String,
    edges: Vec<String>,
}

fn run_sauma_why(query: &str) -> Result<()> {
    let root = locate_project_root()?;
    let artifacts = scan_artifacts(&root)?;
    let seed_edges: Vec<String> = artifacts
        .iter()
        .filter(|artifact| touches(artifact, query))
        .flat_map(|artifact| {
            artifact
                .edges
                .iter()
                .chain(std::iter::once(&artifact.digest))
        })
        .cloned()
        .collect();
    let mut printed = 0;
    for artifact in &artifacts {
        let edge = artifact
            .edges
            .iter()
            .find(|edge| edge.contains(query) || seed_edges.contains(edge))
            .or_else(|| artifact.digest.contains(query).then_some(&artifact.digest));
        if touches(artifact, query) || edge.is_some() {
            let edge = edge.unwrap_or(&artifact.digest);
            println!(
                "{} {} ({}, {}) ← {}",
                artifact.schema,
                short(&artifact.digest),
                artifact.producer,
                short(&artifact.anchored_at),
                short(edge)
            );
            printed += 1;
        }
    }
    if printed == 0 {
        println!("no sauma chain touches {query}");
    }
    Ok(())
}

fn touches(artifact: &Artifact, query: &str) -> bool {
    artifact.digest.contains(query)
        || artifact.anchored_at.contains(query)
        || artifact.producer.contains(query)
        || artifact.edges.iter().any(|edge| edge.contains(query))
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
                collect_artifact(line, &mut artifacts);
            }
            collect_artifact(&raw, &mut artifacts);
        } else {
            collect_artifact(&raw, &mut artifacts);
        }
    }
    Ok(artifacts)
}

fn push_if_exists(files: &mut Vec<PathBuf>, path: PathBuf) {
    if path.exists() {
        files.push(path);
    }
}

fn collect_artifact(raw: &str, artifacts: &mut Vec<Artifact>) {
    let Some(sauma) = object_after(raw, "\"sauma\"") else {
        return;
    };
    let Some(schema) = json_string(&sauma, "schema") else {
        return;
    };
    let Some(digest) = json_string(&sauma, "digest") else {
        return;
    };
    let Some(anchored_at) = json_string(&sauma, "anchored_at") else {
        return;
    };
    if artifacts.iter().any(|artifact| artifact.digest == digest) {
        return;
    }
    let tool = json_string(&sauma, "tool").unwrap_or_else(|| "unknown".to_string());
    let producer = json_string(&sauma, "run")
        .map(|run| format!("{tool}/{run}"))
        .unwrap_or(tool);
    let mut edges = provenance_edges(&sauma);
    edges.push(anchored_at.clone());
    artifacts.push(Artifact {
        schema,
        digest,
        producer,
        anchored_at,
        edges,
    });
}

fn provenance_edges(sauma: &str) -> Vec<String> {
    let Some(array) = array_after(sauma, "\"provenance\"") else {
        return Vec::new();
    };
    let mut edges = Vec::new();
    let mut rest = array.as_str();
    while let Some(object) = object_after(rest, "{") {
        if let Some(oid) = json_string(&object, "oid") {
            edges.push(oid);
        } else if let Some(token) = json_string(&object, "token") {
            edges.push(token);
        } else if let Some(digest) = json_string(&object, "digest") {
            edges.push(digest);
        }
        let next = rest.find(&object).map(|index| index + object.len());
        rest = next.and_then(|index| rest.get(index..)).unwrap_or("");
    }
    edges
}

fn object_after(raw: &str, marker: &str) -> Option<String> {
    let start = raw.find(marker)?;
    let brace = raw[start..].find('{')? + start;
    balanced(raw, brace, '{', '}')
}

fn array_after(raw: &str, marker: &str) -> Option<String> {
    let start = raw.find(marker)?;
    let bracket = raw[start..].find('[')? + start;
    balanced(raw, bracket, '[', ']')
}

fn balanced(raw: &str, start: usize, open: char, close: char) -> Option<String> {
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in raw[start..].char_indices() {
        if in_string {
            escaped = ch == '\\' && !escaped;
            if ch == '"' && !escaped {
                in_string = false;
            } else if ch != '\\' {
                escaped = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
        } else if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Some(raw[start..start + offset + ch.len_utf8()].to_string());
            }
        }
    }
    None
}

fn json_string(raw: &str, key: &str) -> Option<String> {
    let key = format!("\"{key}\"");
    let start = raw.find(&key)?;
    let after_colon = raw[start + key.len()..].find(':')? + start + key.len() + 1;
    let value_start = raw[after_colon..].find(|ch: char| !ch.is_ascii_whitespace())? + after_colon;
    if raw[value_start..].chars().next()? != '"' {
        return None;
    }
    let quote = value_start + 1;
    let end = raw[quote..].find('"')? + quote;
    Some(raw[quote..end].to_string())
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
