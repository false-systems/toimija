//! `.toimija/current.*` — the live agent packet Toimija owns.
//!
//! `refresh` recomputes the context packet and writes it to two files:
//! `.toimija/current.md` (budgeted Markdown for humans/agents) and
//! `.toimija/current.json` (the full machine-readable packet). These are the
//! ONLY paths it mutates — Toimija owns live context under `.toimija/`, never
//! repo policy files. The recomputation itself is read-only and runs no
//! validation commands and makes no network calls.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::command::CommandRunner;
use crate::config::DEFAULT_BUDGET;
use crate::context::{self, ContextPacket};
use crate::render;

/// Live agent packet, Markdown form. Regenerated each refresh; not committed.
pub const CURRENT_MD: &str = ".toimija/current.md";
/// Live agent packet, machine-readable form. Regenerated each refresh.
pub const CURRENT_JSON: &str = ".toimija/current.json";

pub struct Refreshed {
    pub packet: ContextPacket,
    pub markdown: String,
    pub json: String,
    pub md_path: PathBuf,
    pub json_path: PathBuf,
}

/// What a refresh should produce: the budget knobs plus an optional per-session
/// target. `session_dir = Some(dir)` writes the per-session packet (and mirrors
/// the repo-level summary); `None` writes only the repo-level packet. Bundled so
/// it travels as one value through `run` and the watch loop.
#[derive(Clone, Debug)]
pub struct Plan {
    pub for_diff: bool,
    pub budget: Option<usize>,
    pub session_dir: Option<PathBuf>,
}

impl Plan {
    /// Execute this plan against `repo_root`, dispatching to the per-session or
    /// repo-level writer.
    pub fn run(&self, runner: &impl CommandRunner, repo_root: &Path) -> Result<Refreshed> {
        match &self.session_dir {
            Some(dir) => refresh_session(runner, repo_root, self.for_diff, self.budget, dir),
            None => refresh(runner, repo_root, self.for_diff, self.budget),
        }
    }
}

/// Recompute the context packet for `repo_root` and write it to the repo-level
/// `.toimija/current.md` and `.toimija/current.json`. Those two files are the
/// only paths mutated. Used by the sessionless commands (`enter`, `refresh`,
/// `watch`, `context`).
pub fn refresh(
    runner: &impl CommandRunner,
    repo_root: &Path,
    for_diff: bool,
    budget_override: Option<usize>,
) -> Result<Refreshed> {
    let (packet, markdown, json) = compute(runner, repo_root, for_diff, budget_override)?;
    let md_path = repo_root.join(CURRENT_MD);
    let json_path = repo_root.join(CURRENT_JSON);
    write_packet(&md_path, &json_path, &markdown, &json)?;
    Ok(Refreshed {
        packet,
        markdown,
        json,
        md_path,
        json_path,
    })
}

/// Recompute the packet and write it to a per-session directory
/// (`session_dir/current.{md,json}`) — the authoritative packet for a wrapped
/// agent — and also mirror it to the repo-level `.toimija/current.*` summary
/// (demoted, last-writer-wins). Used by `run`'s entry, watch loop, and exit.
pub fn refresh_session(
    runner: &impl CommandRunner,
    repo_root: &Path,
    for_diff: bool,
    budget_override: Option<usize>,
    session_dir: &Path,
) -> Result<Refreshed> {
    let (packet, markdown, json) = compute(runner, repo_root, for_diff, budget_override)?;
    let md_path = session_dir.join("current.md");
    let json_path = session_dir.join("current.json");
    write_packet(&md_path, &json_path, &markdown, &json)?;
    // Repo-level latest summary, kept for sessionless readers (last writer wins).
    write_packet(
        &repo_root.join(CURRENT_MD),
        &repo_root.join(CURRENT_JSON),
        &markdown,
        &json,
    )?;
    Ok(Refreshed {
        packet,
        markdown,
        json,
        md_path,
        json_path,
    })
}

/// Build the packet and its two renderings. The read-only half of a refresh:
/// no validation, no network.
fn compute(
    runner: &impl CommandRunner,
    repo_root: &Path,
    for_diff: bool,
    budget_override: Option<usize>,
) -> Result<(ContextPacket, String, String)> {
    let packet = context::build_context_packet(runner, repo_root, for_diff)
        .context("failed to build context packet")?;
    let budget = budget_override
        .or(packet.config_default_budget())
        .unwrap_or(DEFAULT_BUDGET);
    let markdown = render::render_context_markdown(&packet, budget);
    let json = serde_json::to_string_pretty(&packet)?;
    Ok((packet, markdown, json))
}

/// Write a packet pair, creating the parent directory if needed. The JSON gets a
/// trailing newline so the file is a clean, diff-friendly text artifact.
fn write_packet(md_path: &Path, json_path: &Path, markdown: &str, json: &str) -> Result<()> {
    if let Some(parent) = md_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(md_path, markdown)
        .with_context(|| format!("failed to write {}", md_path.display()))?;
    fs::write(json_path, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", json_path.display()))?;
    Ok(())
}
