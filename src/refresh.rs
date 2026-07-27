//! Live packet writers for repo-level and per-session context.
//!
//! `refresh` recomputes the context packet and writes it to two files:
//! `.toimija/current.md` (budgeted Markdown for humans/agents) and
//! `.toimija/current.json` (the full machine-readable packet). `refresh_session`
//! writes the same packet under `.toimija/sessions/<id>/` and mirrors it to the
//! repo-level summary. Toimija owns live context under `.toimija/`, never repo
//! policy files. The recomputation itself is read-only and runs no validation
//! commands and makes no network calls.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::command::CommandRunner;
use crate::config::DEFAULT_BUDGET;
use crate::context::{self, ContextPacket};
use crate::render;
use crate::session;

static NEXT_ARTIFACT: AtomicU64 = AtomicU64::new(0);

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
    let (packet, markdown, json) = compute(runner, repo_root, for_diff, budget_override, None)?;
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
    let (packet, markdown, json) = compute(
        runner,
        repo_root,
        for_diff,
        budget_override,
        Some(session_dir),
    )?;
    let md_path = session_dir.join("current.md");
    let json_path = session_dir.join("current.json");
    // The session packet is authoritative for the agent — its failure is fatal.
    write_packet(&md_path, &json_path, &markdown, &json)?;
    let context_path = session_dir.join(crate::semantic::CONTEXT_FILE);
    let execution = packet
        .session
        .as_ref()
        .and_then(|session| session.execution.as_ref());
    if let Some(execution) = execution {
        crate::semantic::write_json(&context_path, execution)?;
    } else {
        let _ = fs::remove_file(context_path);
    }
    let point = match execution {
        Some(execution) => execution.repository.clone(),
        None => crate::semantic::repository_point(runner, &packet)?,
    };
    crate::semantic::write_json(
        &session_dir.join(crate::semantic::POINT_FILE),
        &crate::semantic::point_envelope(point),
    )?;
    // The repo-level summary is demoted (last-writer-wins) and best-effort: a
    // failure mirroring it must not abort the already-written session packet, or
    // a transient summary write error would freeze the agent's live context.
    if let Err(error) = write_packet(
        &repo_root.join(CURRENT_MD),
        &repo_root.join(CURRENT_JSON),
        &markdown,
        &json,
    ) {
        eprintln!(
            "toimija: could not update the repo-level summary (session packet is fresh): {error:#}"
        );
    }
    Ok(Refreshed {
        packet,
        markdown,
        json,
        md_path,
        json_path,
    })
}

/// Build the packet and its two renderings. The read-only half of a refresh:
/// no validation, no network. When `session_dir` is given, the session's
/// identity (intent, scope — issue #84) is read from its marker and carried
/// into the packet before rendering, so `run --intent` reaches a wrapped
/// harness and `toimija intent` updates propagate on the next refresh.
/// Best-effort: a missing or unreadable marker yields a packet without the
/// session block, never a failed refresh.
fn compute(
    runner: &impl CommandRunner,
    repo_root: &Path,
    for_diff: bool,
    budget_override: Option<usize>,
    session_dir: Option<&Path>,
) -> Result<(ContextPacket, String, String)> {
    let mut packet = context::build_context_packet(runner, repo_root, for_diff)
        .context("failed to build context packet")?;
    let artifact_id = artifact_id();
    packet.artifact_id = Some(artifact_id.clone());
    let generation = artifact_id
        .split('-')
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    if let Some(dir) = session_dir
        && let Ok(marker) = session::load_marker(dir)
    {
        let mut plugins = std::collections::BTreeMap::new();
        for plugin in crate::plugins::all() {
            // Plugins are enhancements, never requirements: a failing tool
            // degrades the packet with a warning instead of failing the
            // refresh (and with it `run`'s entry, watch loop, and exit
            // summary).
            match plugin.observe(repo_root) {
                Ok(Some(observation)) => {
                    plugins.insert(plugin.meta().name.to_string(), observation);
                }
                Ok(None) => {}
                Err(error) => packet.warnings.push(format!(
                    "Plugin `{}` observation failed; packet compiled without it: {error:#}",
                    plugin.meta().name
                )),
            }
        }
        let teko = plugins.get("teko").cloned();
        packet.session = Some(context::SessionBlock {
            id: marker.id,
            intent: marker.intent,
            scope: marker.scope,
            plugins,
            teko,
            execution: None,
            entry_branch: marker.branch,
            entry_head: marker.head,
        });
        match crate::plugins::teko::active_work(repo_root) {
            Ok(Some(active)) => {
                let execution =
                    crate::semantic::execution_context(runner, &packet, active, generation)?;
                packet
                    .session
                    .as_mut()
                    .expect("session was just installed")
                    .execution = Some(execution);
            }
            Ok(None) => {}
            Err(error) => packet.warnings.push(format!(
                "Plugin `teko` semantic context failed; packet compiled without it: {error:#}"
            )),
        }
    }
    let run = packet.session.as_ref().map(|session| session.id.clone());
    packet.attach_sauma(run)?;
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
    // JSON is authoritative if the process stops between these two atomic
    // replacements; the shared artifact id makes that mismatch detectable.
    crate::atomic_write::replace(md_path, markdown.as_bytes())
        .with_context(|| format!("failed to write {}", md_path.display()))?;
    crate::atomic_write::replace(json_path, format!("{json}\n").as_bytes())
        .with_context(|| format!("failed to write {}", json_path.display()))?;
    Ok(())
}

fn artifact_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let sequence = NEXT_ARTIFACT.fetch_add(1, Ordering::Relaxed);
    format!("{nanos}-{}-{sequence}", std::process::id())
}
