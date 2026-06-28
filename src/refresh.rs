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

/// Recompute the context packet for `repo_root` and write it to
/// `.toimija/current.md` and `.toimija/current.json`. Those two files are the
/// only paths mutated.
pub fn refresh(
    runner: &impl CommandRunner,
    repo_root: &Path,
    for_diff: bool,
    budget_override: Option<usize>,
) -> Result<Refreshed> {
    let packet = context::build_context_packet(runner, repo_root, for_diff)
        .context("failed to build context packet")?;
    let budget = budget_override
        .or(packet.config_default_budget())
        .unwrap_or(DEFAULT_BUDGET);
    let markdown = render::render_context_markdown(&packet, budget);
    let json = serde_json::to_string_pretty(&packet)?;

    let dir = repo_root.join(".toimija");
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let md_path = repo_root.join(CURRENT_MD);
    let json_path = repo_root.join(CURRENT_JSON);
    fs::write(&md_path, &markdown)
        .with_context(|| format!("failed to write {}", md_path.display()))?;
    // Trailing newline so the file is a clean, diff-friendly text artifact.
    fs::write(&json_path, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", json_path.display()))?;

    Ok(Refreshed {
        packet,
        markdown,
        json,
        md_path,
        json_path,
    })
}
