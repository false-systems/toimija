//! Live packet writers for repo-level and per-session context.
//!
//! `refresh` recomputes the context packet and writes it to two files:
//! `.toimija/current.md` (budgeted Markdown for humans/agents) and
//! `.toimija/current.json` (the full machine-readable packet). `refresh_session`
//! writes the same packet under `.toimija/sessions/<id>/` and mirrors it to the
//! repo-level summary. Toimija owns live context under `.toimija/`, never repo
//! policy files. The recomputation itself is read-only and runs no validation
//! commands and makes no network calls.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tracing::warn;

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
#[derive(Clone)]
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

    /// Say in the packet that refreshing it failed, so an agent reading it is
    /// never handed a stale packet that looks fresh (issue #229). The notice
    /// heads the Markdown packet this plan writes — the session packet and its
    /// repo-level mirror, or the repo-level packet — above the last good
    /// content, which is kept. It replaces an earlier notice rather than
    /// stacking, and the next successful refresh rewrites the packet without
    /// it. The JSON packet is left as written: it is what tools verify.
    ///
    /// `Ok(false)` when the packet already carries this failure, so one that
    /// repeats on every poll is written, and reported, once.
    pub fn mark_stale(&self, repo_root: &Path, error: &anyhow::Error) -> Result<bool> {
        let reason = format!("{error:#}").replace('\n', " ");
        let primary = match &self.session_dir {
            Some(dir) => dir.join("current.md"),
            None => repo_root.join(CURRENT_MD),
        };
        let changed = mark_stale_at(repo_root, &primary, &reason)?;
        if changed && self.session_dir.is_some() {
            // The demoted mirror is stale too; best-effort, like its writes.
            let _ = mark_stale_at(repo_root, &repo_root.join(CURRENT_MD), &reason);
        }
        Ok(changed)
    }
}

const STALE_BEGIN: &str = "<!-- toimija:stale:begin -->";
const STALE_END: &str = "<!-- toimija:stale:end -->";

/// Put the stale notice for `reason` at the top of the Markdown packet at
/// `md_path`, replacing any earlier notice. `Ok(false)` when it already says
/// exactly this.
fn mark_stale_at(repo_root: &Path, md_path: &Path, reason: &str) -> Result<bool> {
    let current = match std::fs::read_to_string(md_path) {
        Ok(current) => current,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", md_path.display()));
        }
    };
    let body = match current
        .strip_prefix(STALE_BEGIN)
        .and_then(|rest| rest.split_once(STALE_END))
    {
        Some((notice, body)) => {
            if notice.contains(reason) {
                return Ok(false);
            }
            body.trim_start_matches('\n')
        }
        None => current.as_str(),
    };
    let since = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "an unknown time".to_owned());
    let notice = format!(
        "{STALE_BEGIN}\n\
         > **Refresh failed — this packet is stale.** Since {since}, refreshing it fails:\n\
         > {reason}\n\
         > Everything below is from the last successful refresh. Fix the cause; the next \
         successful refresh replaces this packet.\n\
         {STALE_END}\n\n"
    );
    crate::owned::replace(repo_root, md_path, format!("{notice}{body}").as_bytes())?;
    Ok(true)
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
    let computed = compute(runner, repo_root, for_diff, budget_override, None)?;
    let md_path = repo_root.join(CURRENT_MD);
    let json_path = repo_root.join(CURRENT_JSON);
    write_packet(
        repo_root,
        &md_path,
        &json_path,
        &computed.markdown,
        &computed.json,
    )?;
    Ok(Refreshed {
        packet: computed.packet,
        markdown: computed.markdown,
        json: computed.json,
        md_path,
        json_path,
    })
}

/// Recompute the packet and write it to a per-session directory
/// (`session_dir/current.{md,json}`) — the authoritative packet for a wrapped
/// agent — and also mirror it to the repo-level `.toimija/current.*` summary
/// (demoted, last-writer-wins). Used by `run`'s entry, watch loop, and exit.
///
/// The mirror carries the session block only while the session is open. The
/// exit refresh runs after the marker is closed, and then the repo-level
/// summary gets the same final facts without the block: a closed session's
/// id, intent and scope must not survive in the packet the next agent reads.
pub fn refresh_session(
    runner: &impl CommandRunner,
    repo_root: &Path,
    for_diff: bool,
    budget_override: Option<usize>,
    session_dir: &Path,
) -> Result<Refreshed> {
    let Computed {
        packet,
        markdown,
        json,
        session_closed,
        point,
    } = compute(
        runner,
        repo_root,
        for_diff,
        budget_override,
        Some(session_dir),
    )?;
    let md_path = session_dir.join("current.md");
    let json_path = session_dir.join("current.json");
    // The session packet is authoritative for the agent — its failure is fatal.
    write_packet(repo_root, &md_path, &json_path, &markdown, &json)?;
    let point_file = session_dir.join(crate::semantic::POINT_FILE);
    match point {
        Some(point) => crate::semantic::write_json(
            repo_root,
            &point_file,
            &crate::semantic::point_envelope(point),
        )?,
        // An ended session: nothing may read an earlier point as current.
        None => {
            let _ = crate::owned::remove_file(repo_root, &point_file);
        }
    }
    // The repo-level summary is demoted (last-writer-wins) and best-effort: a
    // failure mirroring it must not abort the already-written session packet, or
    // a transient summary write error would freeze the agent's live context.
    let mirror = if session_closed {
        sessionless(&packet, budget_override)
    } else {
        Ok((markdown.clone(), json.clone()))
    };
    let mirrored = mirror.and_then(|(markdown, json)| {
        write_packet(
            repo_root,
            &repo_root.join(CURRENT_MD),
            &repo_root.join(CURRENT_JSON),
            &markdown,
            &json,
        )
    });
    if let Err(error) = mirrored {
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

/// A built packet and its two renderings, plus whether the session it was
/// built for has already closed, and — for a refresh of a live session — the
/// session's repository point, read from the same state as the packet.
struct Computed {
    packet: ContextPacket,
    markdown: String,
    json: String,
    session_closed: bool,
    point: Option<false_agent_protocol::RepositoryPoint>,
}

/// The same packet with no session block, sealed and rendered afresh: what the
/// repo-level summary holds once the session that mirrored into it has ended.
fn sessionless(packet: &ContextPacket, budget_override: Option<usize>) -> Result<(String, String)> {
    let mut packet = packet.clone();
    packet.session = None;
    packet.attach_sauma(None)?;
    Ok(render_pair(&packet, budget_override)?)
}

fn render_pair(
    packet: &ContextPacket,
    budget_override: Option<usize>,
) -> serde_json::Result<(String, String)> {
    let budget = budget_override
        .or(packet.config_default_budget())
        .unwrap_or(DEFAULT_BUDGET);
    let markdown = render::render_context_markdown(packet, budget);
    let json = serde_json::to_string_pretty(packet)?;
    Ok((markdown, json))
}

/// Build the packet and its two renderings. The read-only half of a refresh:
/// no validation, no network. When `session_dir` is given, the session's
/// identity (intent, scope — issue #84) is read from its marker and carried
/// into the packet before rendering, so `run --intent` reaches a wrapped
/// harness and `toimija intent` updates propagate on the next refresh.
/// Best-effort: a missing or unreadable marker yields a packet without the
/// session block, never a failed refresh — but unreadable ≠ missing (issue
/// #161): only `NotFound` is quiet; a marker that exists and cannot be read
/// is warned about, because the packet it feeds silently loses its session
/// identity.
///
/// A session refresh pairs the packet with a repository point. If the
/// worktree moves while the point's tree is written, the whole packet is built
/// again from a fresh read, a bounded number of times
/// ([`crate::semantic::retry_moved`]), and then the refresh fails — the packet
/// never carries a tree from one moment beside a token from another.
fn compute(
    runner: &impl CommandRunner,
    repo_root: &Path,
    for_diff: bool,
    budget_override: Option<usize>,
    session_dir: Option<&Path>,
) -> Result<Computed> {
    crate::semantic::retry_moved(|| {
        compute_once(runner, repo_root, for_diff, budget_override, session_dir)
    })
}

fn compute_once(
    runner: &impl CommandRunner,
    repo_root: &Path,
    for_diff: bool,
    budget_override: Option<usize>,
    session_dir: Option<&Path>,
) -> Result<Computed> {
    let mut packet = context::build_context_packet(runner, repo_root, for_diff)
        .context("failed to build context packet")?;
    let artifact_id = artifact_id();
    packet.artifact_id = Some(artifact_id.clone());
    let generation = artifact_id
        .split('-')
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let marker = match session_dir.map(session::load_marker) {
        Some(Ok(marker)) => Some(marker),
        Some(Err(error)) => {
            let missing = error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound);
            if !missing {
                // `:#` prints the whole context chain — the cause (EACCES,
                // EIO, parse failure) matters as much as the path.
                warn!(event = "toimija.session.marker_unreadable", error = %format!("{error:#}"));
            }
            None
        }
        None => None,
    };
    // A session whose `run` was killed before closing its marker has ended
    // too: its brief stays in its own packet, never in the repo mirror.
    let session_closed = marker.as_ref().is_some_and(|marker| {
        marker.status == session::SessionStatus::Closed
            || crate::sessions::process_gone(repo_root, marker)
    });
    let mut point = None;
    if let Some(marker) = marker {
        packet.session = Some(context::SessionBlock {
            id: marker.id,
            intent: marker.intent,
            scope: marker.scope,
            execution: None,
            entry_branch: marker.branch,
            entry_head: marker.head,
        });
        // A session that has ended stands at no repository point (`run`
        // removes its point file as it exits), so none is computed for it: no
        // tree is written only to be deleted.
        if !session_closed {
            let execution = crate::semantic::execution_context(runner, &packet, generation)?;
            point = Some(execution.repository.clone());
            packet
                .session
                .as_mut()
                .expect("session was just installed")
                .execution = Some(execution);
        }
    } else if session_dir.is_some() {
        // A session directory without a readable marker still gets its point.
        point = Some(crate::semantic::repository_point(runner, &packet)?);
    }
    let run = packet.session.as_ref().map(|session| session.id.clone());
    packet.attach_sauma(run)?;
    let (markdown, json) = render_pair(&packet, budget_override)?;
    Ok(Computed {
        packet,
        markdown,
        json,
        session_closed,
        point,
    })
}

/// Write a packet pair, creating the parent directory if needed — never
/// through a symlink (see `owned`). The JSON gets a trailing newline so the
/// file is a clean, diff-friendly text artifact.
fn write_packet(
    repo_root: &Path,
    md_path: &Path,
    json_path: &Path,
    markdown: &str,
    json: &str,
) -> Result<()> {
    let parent = md_path.parent().context("a packet file has a parent")?;
    crate::owned::ensure_dir(repo_root, parent).context("failed to write the live packet")?;
    // JSON is authoritative if the process stops between these two atomic
    // replacements; the shared artifact id makes that mismatch detectable.
    crate::owned::replace(repo_root, md_path, markdown.as_bytes())?;
    crate::owned::replace(repo_root, json_path, format!("{json}\n").as_bytes())?;
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
