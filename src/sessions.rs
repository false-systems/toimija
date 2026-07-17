//! `toimija sessions` — read-only awareness of sibling sessions in this repo.
//!
//! Presence answers: who is here?  Intent answers: why are they here?
//! Scope answers: what might they touch?  Overlap answers: should another agent
//! be careful?
//!
//! This command only *reads* `.toimija/sessions/*/session.json`, checks
//! best-effort process liveness, and renders. It assigns nothing, signals
//! nothing, kills nothing, and touches no network. Toimija coordinates
//! visibility, not decisions; the human remains the orchestrator.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;
use tracing::{debug, info};

use crate::command::CommandRunner;
use crate::coverage::Coverage;
use crate::footprint::{self, HistoryEnd};
use crate::handoff;
use crate::session::{SESSIONS_SUBDIR, SessionMarker, SessionStatus};

/// A session open longer than this with no live process is treated as stale.
/// 90 minutes (decision: start simple; config can come later, not a project).
pub const DEFAULT_STALE_AFTER_SECS: u64 = 90 * 60;

/// Closed or stale sessions older than this are removed by `prune`. 7 days —
/// long enough that recent history stays visible in listings, short enough that
/// a busy repo does not accumulate session directories without bound.
pub const DEFAULT_RETENTION_SECS: u64 = 7 * 24 * 60 * 60;

/// Resolved liveness of a session, derived from its marker plus a best-effort
/// process check — never stored, always recomputed when listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// Marker open and the pid appears alive.
    Active,
    /// Marker open but the pid is gone, or it has been open past the threshold.
    Stale,
    /// Marker explicitly closed.
    Closed,
}

/// Severity of an overlap warning. Deliberately only two levels, and both are
/// *descriptive*: they report a situation, they never prescribe an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
}

/// One advisory observation about the live session set. Never an instruction —
/// "two live sessions on the same branch", not "codex should stop".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Warning {
    pub severity: Severity,
    pub message: String,
}

/// A session as presented by `toimija sessions`: its marker, resolved liveness,
/// and the path to its live packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionView {
    #[serde(flatten)]
    pub marker: SessionMarker,
    pub liveness: Liveness,
    pub packet: String,
    pub handoffs: usize,
}

impl SessionView {
    /// Short label for messages: the first argv element (the agent program).
    pub fn agent_label(&self) -> &str {
        self.marker
            .agent
            .first()
            .map(String::as_str)
            .unwrap_or("unknown")
    }
}

/// The full, renderable result of a `sessions` listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionsReport {
    pub sessions: Vec<SessionView>,
    pub warnings: Vec<Warning>,
    /// Coverage of the marker scan itself (R10, TA-005): `available` means
    /// every sibling marker was read; `degraded` means at least one marker is
    /// unreadable or malformed, so the listing may be missing a live sibling.
    pub coverage: Coverage,
}

/// Read every session marker under `.toimija/sessions/`, resolve liveness, and
/// compute overlap warnings. Read-only; a single malformed marker is skipped
/// (logged), not fatal — one bad file must not blind an agent to its siblings.
pub fn report(
    runner: &dyn CommandRunner,
    repo_root: &Path,
    stale_after_secs: u64,
) -> Result<SessionsReport> {
    let now = now_secs();
    let mut sessions = Vec::new();
    let scan = read_markers(repo_root)?;
    for (_dir, marker) in scan.markers {
        let alive = pid_is_alive(runner, repo_root, marker.pid);
        let liveness = liveness(&marker, now, alive, stale_after_secs);
        debug!(
            event = "toimija.session.liveness_checked",
            id = %marker.id,
            pid = marker.pid,
            alive,
            liveness = ?liveness
        );
        let packet = packet_path(repo_root, &marker.id);
        let handoffs = handoff::count(repo_root, &marker.id).unwrap_or(0);
        sessions.push(SessionView {
            marker,
            liveness,
            packet,
            handoffs,
        });
    }
    // Stable order: newest first by start time, then id.
    sessions.sort_by(|a, b| {
        b.marker
            .started_at
            .cmp(&a.marker.started_at)
            .then_with(|| a.marker.id.cmp(&b.marker.id))
    });

    let mut warnings = overlap_warnings(&sessions);
    // A sibling that cannot be read is a blind spot, never a quiet absence:
    // the human sees which marker is broken instead of a shorter listing.
    for unreadable in &scan.unreadable {
        warnings.push(Warning {
            severity: Severity::Warning,
            message: format!("session marker unreadable: {unreadable}"),
        });
    }
    let coverage = if scan.unreadable.is_empty() {
        Coverage::available(MARKER_SCAN_SOURCE)
    } else {
        Coverage::degraded(
            MARKER_SCAN_SOURCE,
            format!(
                "{} session marker(s) unreadable; the listing may be missing a live sibling",
                scan.unreadable.len()
            ),
        )
    };
    info!(
        event = "toimija.sessions.listed",
        total = sessions.len(),
        warnings = warnings.len(),
        unreadable = scan.unreadable.len()
    );
    if !warnings.is_empty() {
        info!(
            event = "toimija.sessions.overlap_detected",
            count = warnings.len()
        );
    }
    Ok(SessionsReport {
        sessions,
        warnings,
        coverage,
    })
}

/// The coverage source label for the sibling-marker scan.
const MARKER_SCAN_SOURCE: &str = "session marker scan (.toimija/sessions/*/session.json)";

/// The result of scanning `.toimija/sessions/`: the markers that parsed, and a
/// safe diagnostic for each one that did not. Unreadable is not the same as
/// absent (R10, TA-005) — a directory without a `session.json` is not a
/// session, but a marker that exists and cannot be read is a blind spot.
struct MarkerScan {
    markers: Vec<(PathBuf, SessionMarker)>,
    unreadable: Vec<String>,
}

/// Read and parse all `session.json` markers, each with the session directory
/// it was found in. Missing directory → empty scan.
fn read_markers(repo_root: &Path) -> Result<MarkerScan> {
    let dir = repo_root.join(SESSIONS_SUBDIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MarkerScan {
                markers: Vec::new(),
                unreadable: Vec::new(),
            });
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", dir.display()));
        }
    };
    let mut markers = Vec::new();
    let mut unreadable = Vec::new();
    for entry in entries {
        let entry = entry?;
        let marker_path = entry.path().join("session.json");
        let raw = match fs::read_to_string(&marker_path) {
            Ok(raw) => raw,
            // No marker in this directory: not a session, skip quietly.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                debug!(
                    event = "toimija.session.marker_unreadable",
                    path = %marker_path.display(),
                    error = %error
                );
                unreadable.push(format!(
                    "{} (read error: {})",
                    marker_path.display(),
                    error.kind()
                ));
                continue;
            }
        };
        match serde_json::from_str::<SessionMarker>(&raw) {
            Ok(marker) => markers.push((entry.path(), marker)),
            Err(error) => {
                debug!(
                    event = "toimija.session.marker_unreadable",
                    path = %marker_path.display(),
                    error = %error
                );
                unreadable.push(format!("{} (malformed JSON)", marker_path.display()));
            }
        }
    }
    // read_dir order is platform-arbitrary; sort so diagnostics are stable.
    unreadable.sort();
    Ok(MarkerScan {
        markers,
        unreadable,
    })
}

/// Resolve liveness from the marker, a process-liveness flag, and the staleness
/// threshold. Pure, so the staleness/active/closed transitions are unit-tested
/// without touching the OS or the clock.
///
/// The window measures **inactivity**, not lifetime (proposal 0002): a
/// day-long session whose watcher keeps stamping `last_seen_at` stays active;
/// one whose heartbeat froze goes stale within the window even if a recycled
/// pid looks alive. Markers from before the heartbeat fall back to
/// `started_at` — exactly the old D2 behavior.
pub fn liveness(
    marker: &SessionMarker,
    now_secs: u64,
    pid_alive: bool,
    stale_after_secs: u64,
) -> Liveness {
    match marker.status {
        SessionStatus::Closed => Liveness::Closed,
        SessionStatus::Active => {
            let last_seen = marker.last_seen_at.unwrap_or(marker.started_at);
            let idle = now_secs.saturating_sub(last_seen);
            if !pid_alive || idle > stale_after_secs {
                Liveness::Stale
            } else {
                Liveness::Active
            }
        }
    }
}

/// The result of a prune pass: which session ids were removed, how many stayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PruneReport {
    pub removed: Vec<String>,
    pub kept: usize,
}

/// Remove closed or stale session directories older than the retention window.
/// Live sessions are never touched; a session must *both* be past liveness and
/// past retention to go. Synchronous, local, and only during an explicitly
/// invoked command — this is a cleanup pass, not a daemon.
pub fn prune(
    runner: &dyn CommandRunner,
    repo_root: &Path,
    stale_after_secs: u64,
    retention_secs: u64,
) -> Result<PruneReport> {
    let now = now_secs();
    let mut removed = Vec::new();
    // One bounded journal scan per pass. A session already remembered at
    // teardown is removed without writing a duplicate record.
    let mut recorded = footprint::recorded_session_ids(repo_root)?;
    // Unreadable markers are never pruned: deletion demands the certain
    // signal, and an unreadable marker gives none. They count as kept.
    let scan = read_markers(repo_root)?;
    let mut kept = scan.unreadable.len();
    for (dir, marker) in scan.markers {
        let alive = pid_is_alive(runner, repo_root, marker.pid);
        let liveness = liveness(&marker, now, alive, stale_after_secs);
        if prunable(liveness, &marker, alive, now, retention_secs) {
            let end = match liveness {
                Liveness::Closed => HistoryEnd::Closed,
                Liveness::Stale => HistoryEnd::ReapedStale,
                Liveness::Active => unreachable!("active sessions are never prunable"),
            };
            footprint::reap(repo_root, &dir, &marker, end, now, &mut recorded)?;
            removed.push(marker.id);
        } else {
            kept += 1;
        }
    }
    // read_dir order is platform-arbitrary; sort so output is deterministic.
    removed.sort();
    info!(
        event = "toimija.sessions.pruned",
        removed = removed.len(),
        kept
    );
    Ok(PruneReport { removed, kept })
}

/// Whether a session directory may be removed. Pure, so the retention decision
/// is unit-tested without a filesystem or clock. Age is measured from the most
/// honest timestamp available: `closed_at` for a clean close, `started_at` for
/// a stale marker that never closed.
///
/// Stale-by-TTL is a *display* judgment ("looks abandoned"); deletion demands
/// the certain signal. A marker can be reported stale purely because it has
/// been open past the TTL while its process — a long interactive session — is
/// alive and mid-work; removing its directory would pull the live packet out
/// from under a running watcher. So a stale marker is prunable only when its
/// pid is gone. `Closed` is an explicit end-of-life record and ages out
/// regardless of `pid_alive`, which by then may point at a recycled pid.
pub fn prunable(
    liveness: Liveness,
    marker: &SessionMarker,
    pid_alive: bool,
    now_secs: u64,
    retention_secs: u64,
) -> bool {
    let reference = match liveness {
        Liveness::Active => return false,
        Liveness::Closed => marker.closed_at.unwrap_or(marker.started_at),
        Liveness::Stale if pid_alive => return false,
        // Retention counts from when the session was last seen alive, not
        // from its birth (proposal 0002).
        Liveness::Stale => marker.last_seen_at.unwrap_or(marker.started_at),
    };
    now_secs.saturating_sub(reference) > retention_secs
}

/// Best-effort process liveness via `kill -0 <pid>` (exit 0 = the process
/// exists and is signalable). Routed through the runner so it is mockable in
/// tests. Best-effort only: a pid owned by another user, or a recycled pid, can
/// mislead — awareness, not authority.
fn pid_is_alive(runner: &dyn CommandRunner, cwd: &Path, pid: u32) -> bool {
    let pid = pid.to_string();
    matches!(
        runner.run(cwd, "kill", &["-0", &pid]),
        Ok(output) if output.status == 0
    )
}

/// Path to a session's live packet, as a display string for the listing.
fn packet_path(repo_root: &Path, id: &str) -> String {
    PathBuf::from(SESSIONS_SUBDIR)
        .join(id)
        .join("current.md")
        .display()
        .to_string()
        // Render relative to the repo, matching how Toimija paths read elsewhere.
        .replace(&format!("{}/", repo_root.display()), "")
}

/// Advisory overlap detection across the session set.
///
/// Every warning here is **descriptive** — it reports a situation ("two live
/// sessions on the same branch"), never an instruction ("codex should stop"). A
/// prescriptive warning would be one step from assignment, the line Toimija does
/// not cross: it coordinates visibility, not decisions. The human reads these and
/// decides; Toimija only makes the situation visible.
fn overlap_warnings(sessions: &[SessionView]) -> Vec<Warning> {
    let live: Vec<&SessionView> = sessions
        .iter()
        .filter(|view| view.liveness == Liveness::Active)
        .collect();
    let mut warnings = Vec::new();

    // Multiple live sessions on the same branch — and, among them, any that
    // declared no intent (you can see who is here, but not why).
    for (branch, group) in group_live_by(&live, |view| view.marker.branch.clone()) {
        if group.len() < 2 {
            continue;
        }
        warnings.push(Warning {
            severity: Severity::Warning,
            message: format!(
                "{} live sessions on branch {branch}: {}",
                group.len(),
                join_labels(&group)
            ),
        });
        for view in group.iter().filter(|view| view.marker.intent.is_none()) {
            warnings.push(Warning {
                severity: Severity::Info,
                message: format!(
                    "{} shares branch {branch} but declared no intent",
                    session_label(view)
                ),
            });
        }
    }

    // Multiple live sessions in the very same worktree — sharing one working
    // tree, where edits are unattributable, not merely a branch name. Markers
    // written before the `worktree` field default to "" — skip those rather than
    // group every old marker together as a false "same worktree" overlap.
    for (worktree, group) in group_live_by(&live, |view| {
        let worktree = view.marker.worktree.trim();
        (!worktree.is_empty()).then(|| worktree.to_string())
    }) {
        if group.len() >= 2 {
            warnings.push(Warning {
                severity: Severity::Warning,
                message: format!(
                    "{} live sessions share worktree {worktree}: {}",
                    group.len(),
                    join_labels(&group)
                ),
            });
        }
    }

    // Live sessions whose declared scopes intersect — exactly or by nesting
    // (`src` and `src/lib.rs` overlap). Component-aware, so `src` does not match
    // `src2`.
    for (index, first) in live.iter().enumerate() {
        for second in live.iter().skip(index + 1) {
            let shared = overlapping_scopes(&first.marker.scope, &second.marker.scope);
            if !shared.is_empty() {
                warnings.push(Warning {
                    severity: Severity::Warning,
                    message: format!(
                        "{} and {} declare overlapping scope: {}",
                        session_label(first),
                        session_label(second),
                        shared.join(", ")
                    ),
                });
            }
        }
    }

    // The same agent program live in more than one session.
    for (agent, group) in group_live_by(&live, |view| Some(view.agent_label().to_string())) {
        if group.len() >= 2 {
            warnings.push(Warning {
                severity: Severity::Info,
                message: format!(
                    "agent {agent} is live in {} sessions: {}",
                    group.len(),
                    group
                        .iter()
                        .map(|view| view.marker.id.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
    }

    // Sessions still marked open whose process is gone (or open past the window).
    for view in sessions
        .iter()
        .filter(|view| view.liveness == Liveness::Stale)
    {
        warnings.push(Warning {
            severity: Severity::Warning,
            message: format!(
                "{} is marked open but looks stale (its process is gone or it has shown no activity past the staleness window)",
                session_label(view)
            ),
        });
    }

    warnings
}

/// Group the live sessions by a string key (skipping any with no key), in a
/// deterministic key-sorted order so the rendered warnings are stable.
fn group_live_by<'a>(
    live: &[&'a SessionView],
    key: impl Fn(&SessionView) -> Option<String>,
) -> BTreeMap<String, Vec<&'a SessionView>> {
    let mut groups: BTreeMap<String, Vec<&'a SessionView>> = BTreeMap::new();
    for view in live {
        if let Some(group_key) = key(view) {
            groups.entry(group_key).or_default().push(view);
        }
    }
    groups
}

/// Declared-scope pairs that intersect, as readable strings: equal paths as
/// themselves, nested paths as "broad ⊃ narrow". Uses `Path::starts_with` (both
/// directions) so matching is component-aware — `src` overlaps `src/lib.rs` but
/// not `src2` — and returns an order-stable, de-duplicated set.
fn overlapping_scopes(first: &[String], second: &[String]) -> Vec<String> {
    let mut shared = std::collections::BTreeSet::new();
    for a in first {
        for b in second {
            let (na, nb) = (normalize_scope(a), normalize_scope(b));
            let (pa, pb) = (Path::new(&na), Path::new(&nb));
            if na == nb {
                shared.insert(na);
            } else if pa.starts_with(pb) {
                // `a` is the deeper path; `b` (nb) is the broader one.
                shared.insert(format!("{nb} ⊃ {na}"));
            } else if pb.starts_with(pa) {
                shared.insert(format!("{na} ⊃ {nb}"));
            }
        }
    }
    shared.into_iter().collect()
}

/// Light scope normalization for comparison: trim, drop a leading `./` and any
/// trailing slash. Not canonicalization — declared scopes are advisory paths,
/// not resolved filesystem locations.
fn normalize_scope(path: &str) -> String {
    path.trim()
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_string()
}

/// "agent (id)" — how a session is named inside a warning message.
fn session_label(view: &SessionView) -> String {
    format!("{} ({})", view.agent_label(), view.marker.id)
}

fn join_labels(group: &[&SessionView]) -> String {
    group
        .iter()
        .map(|view| session_label(view))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render the report as the default human-readable listing: live sessions
/// first, then stale, then closed, then a `WARNINGS` block. Empty repo → a
/// single honest line so the agent knows it was answered, not skipped.
pub fn render_text(report: &SessionsReport) -> String {
    if report.sessions.is_empty() {
        return "No sessions recorded under .toimija/sessions/.\n".to_string();
    }
    let now = now_secs();
    let mut out = String::new();
    for (heading, want) in [
        ("LIVE", Liveness::Active),
        ("STALE", Liveness::Stale),
        ("CLOSED", Liveness::Closed),
    ] {
        let group: Vec<&SessionView> = report
            .sessions
            .iter()
            .filter(|view| view.liveness == want)
            .collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(heading);
        out.push('\n');
        for view in group {
            let m = &view.marker;
            out.push_str(&format!("  {}  {}\n", view.agent_label(), m.id));
            out.push_str(&format!(
                "    branch: {}\n",
                m.branch.as_deref().unwrap_or("unknown")
            ));
            out.push_str(&format!("    worktree: {}\n", m.worktree));
            out.push_str(&format!(
                "    intent: {}\n",
                m.intent.as_deref().unwrap_or("unknown")
            ));
            let scope = if m.scope.is_empty() {
                "unknown".to_string()
            } else {
                m.scope.join(", ")
            };
            out.push_str(&format!("    scope: {scope}\n"));
            out.push_str(&format!(
                "    started: {} ago\n",
                humanize_age(now.saturating_sub(m.started_at))
            ));
            if let Some(last_seen) = m.last_seen_at {
                out.push_str(&format!(
                    "    seen: {} ago\n",
                    humanize_age(now.saturating_sub(last_seen))
                ));
            }
            out.push_str(&format!("    packet: {}\n", view.packet));
            if view.handoffs > 0 {
                out.push_str(&format!("    handoff: {} record(s)\n", view.handoffs));
            }
        }
        out.push('\n');
    }
    if !report.warnings.is_empty() {
        out.push_str("WARNINGS\n");
        for warning in &report.warnings {
            out.push_str(&format!("  - {}\n", warning.message));
        }
    }
    out
}

/// Coarse, dependency-free humanizing of a duration in seconds. Awareness only
/// needs "12m", not calendar precision.
fn humanize_age(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// Seconds since the Unix epoch; 0 if the clock is before the epoch.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(id: &str, status: SessionStatus, started_at: u64) -> SessionMarker {
        SessionMarker {
            id: id.to_string(),
            pid: 4242,
            agent: vec!["codex".to_string()],
            worktree: "/repo".to_string(),
            branch: Some("feat/x".to_string()),
            head: None,
            intent: None,
            scope: Vec::new(),
            started_at,
            last_seen_at: None,
            status,
            closed_at: None,
            exit_status: None,
        }
    }

    #[test]
    fn marker_scan_reports_malformed_markers_instead_of_skipping_them() {
        let temp = tempfile::tempdir().unwrap();
        let sessions_dir = temp.path().join(SESSIONS_SUBDIR);
        let good = sessions_dir.join("100-1");
        fs::create_dir_all(&good).unwrap();
        fs::write(
            good.join("session.json"),
            serde_json::to_string(&marker("100-1", SessionStatus::Closed, 100)).unwrap(),
        )
        .unwrap();
        let bad = sessions_dir.join("100-2");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("session.json"), "{not json").unwrap();
        // A directory without a marker is not a session — quiet skip, not a
        // blind spot.
        fs::create_dir_all(sessions_dir.join("100-3")).unwrap();

        let scan = read_markers(temp.path()).unwrap();

        assert_eq!(scan.markers.len(), 1);
        assert_eq!(scan.markers[0].1.id, "100-1");
        assert_eq!(scan.unreadable.len(), 1);
        assert!(scan.unreadable[0].contains("100-2"));
        assert!(scan.unreadable[0].contains("malformed JSON"));
    }

    #[test]
    fn closed_marker_is_always_closed() {
        let m = marker("a", SessionStatus::Closed, 0);
        assert_eq!(
            liveness(&m, 1_000, true, DEFAULT_STALE_AFTER_SECS),
            Liveness::Closed
        );
    }

    #[test]
    fn open_marker_with_live_pid_is_active() {
        let m = marker("a", SessionStatus::Active, 1_000);
        assert_eq!(
            liveness(&m, 1_100, true, DEFAULT_STALE_AFTER_SECS),
            Liveness::Active
        );
    }

    #[test]
    fn open_marker_with_dead_pid_is_stale() {
        let m = marker("a", SessionStatus::Active, 1_000);
        assert_eq!(
            liveness(&m, 1_100, false, DEFAULT_STALE_AFTER_SECS),
            Liveness::Stale
        );
    }

    #[test]
    fn open_marker_past_threshold_is_stale_even_if_pid_alive() {
        // No heartbeat recorded (a pre-0002 marker): inactivity falls back to
        // started_at — exactly the old D2 behavior.
        let m = marker("a", SessionStatus::Active, 1_000);
        // now is well past started_at + threshold.
        assert_eq!(
            liveness(
                &m,
                1_000 + DEFAULT_STALE_AFTER_SECS + 1,
                true,
                DEFAULT_STALE_AFTER_SECS
            ),
            Liveness::Stale
        );
    }

    #[test]
    fn long_session_with_a_recent_heartbeat_stays_active() {
        // The proposal-0002 regression case: a session open for days whose
        // watcher keeps stamping last_seen_at is LIVE, not stale.
        let mut m = marker("a", SessionStatus::Active, 1_000);
        let now = 1_000 + 30 * DEFAULT_STALE_AFTER_SECS;
        m.last_seen_at = Some(now - 60);
        assert_eq!(
            liveness(&m, now, true, DEFAULT_STALE_AFTER_SECS),
            Liveness::Active
        );
    }

    #[test]
    fn frozen_heartbeat_goes_stale_even_with_a_live_pid() {
        // A recycled pid can look alive; a frozen heartbeat tells the truth.
        let mut m = marker("a", SessionStatus::Active, 1_000);
        m.last_seen_at = Some(2_000);
        assert_eq!(
            liveness(
                &m,
                2_000 + DEFAULT_STALE_AFTER_SECS + 1,
                true,
                DEFAULT_STALE_AFTER_SECS
            ),
            Liveness::Stale
        );
    }

    #[test]
    fn stale_retention_ages_from_the_last_heartbeat_when_present() {
        let mut m = marker("a", SessionStatus::Active, 1_000);
        let last_seen = 1_000 + 5_000;
        m.last_seen_at = Some(last_seen);
        let retention = DEFAULT_RETENTION_SECS;
        // Past retention from started_at but not from last_seen_at → kept.
        assert!(!prunable(
            Liveness::Stale,
            &m,
            false,
            last_seen + retention,
            retention
        ));
        assert!(prunable(
            Liveness::Stale,
            &m,
            false,
            last_seen + retention + 1,
            retention
        ));
    }

    fn view(
        id: &str,
        agent: &str,
        branch: &str,
        liveness: Liveness,
        intent: Option<&str>,
        scope: &[&str],
    ) -> SessionView {
        SessionView {
            marker: SessionMarker {
                id: id.to_string(),
                pid: 1,
                agent: vec![agent.to_string()],
                worktree: "/repo".to_string(),
                branch: Some(branch.to_string()),
                head: None,
                intent: intent.map(str::to_string),
                scope: scope.iter().map(|s| s.to_string()).collect(),
                started_at: 0,
                last_seen_at: None,
                status: match liveness {
                    Liveness::Closed => SessionStatus::Closed,
                    _ => SessionStatus::Active,
                },
                closed_at: None,
                exit_status: None,
            },
            liveness,
            packet: format!(".toimija/sessions/{id}/current.md"),
            handoffs: 0,
        }
    }

    fn has_message(warnings: &[Warning], needle: &str) -> bool {
        warnings.iter().any(|w| w.message.contains(needle))
    }

    #[test]
    fn lone_live_session_warns_about_nothing() {
        let views = [view(
            "1-1",
            "claude",
            "feat/x",
            Liveness::Active,
            Some("ship it"),
            &["src/a.rs"],
        )];
        assert!(overlap_warnings(&views).is_empty());
    }

    #[test]
    fn two_live_sessions_on_one_branch_warn_descriptively() {
        let views = [
            view("1-1", "claude", "feat/x", Liveness::Active, Some("a"), &[]),
            view("2-2", "codex", "feat/x", Liveness::Active, Some("b"), &[]),
        ];
        let warnings = overlap_warnings(&views);
        assert!(has_message(&warnings, "live sessions on branch feat/x"));
        // Descriptive, not prescriptive: it names the situation, not an action.
        assert!(!has_message(&warnings, "should"));
    }

    #[test]
    fn unknown_intent_among_branch_siblings_is_flagged() {
        let views = [
            view("1-1", "claude", "feat/x", Liveness::Active, Some("a"), &[]),
            view("2-2", "codex", "feat/x", Liveness::Active, None, &[]),
        ];
        let warnings = overlap_warnings(&views);
        assert!(has_message(
            &warnings,
            "codex (2-2) shares branch feat/x but declared no intent"
        ));
    }

    #[test]
    fn overlapping_declared_scope_warns() {
        let views = [
            view(
                "1-1",
                "claude",
                "feat/x",
                Liveness::Active,
                Some("a"),
                &["src/a.rs", "src/b.rs"],
            ),
            view(
                "2-2",
                "codex",
                "feat/y",
                Liveness::Active,
                Some("b"),
                &["src/b.rs"],
            ),
        ];
        let warnings = overlap_warnings(&views);
        assert!(has_message(
            &warnings,
            "declare overlapping scope: src/b.rs"
        ));
    }

    #[test]
    fn nested_scope_paths_overlap() {
        // `src` and `src/lib.rs` intersect even though neither string contains
        // the other exactly.
        let views = [
            view(
                "1-1",
                "claude",
                "feat/x",
                Liveness::Active,
                Some("a"),
                &["src"],
            ),
            view(
                "2-2",
                "codex",
                "feat/y",
                Liveness::Active,
                Some("b"),
                &["src/lib.rs"],
            ),
        ];
        let warnings = overlap_warnings(&views);
        assert!(has_message(
            &warnings,
            "overlapping scope: src ⊃ src/lib.rs"
        ));
    }

    #[test]
    fn sibling_prefix_paths_do_not_falsely_overlap() {
        // `src` must not match `src2` — component-aware, not string-prefix.
        let views = [
            view(
                "1-1",
                "claude",
                "feat/x",
                Liveness::Active,
                Some("a"),
                &["src"],
            ),
            view(
                "2-2",
                "codex",
                "feat/y",
                Liveness::Active,
                Some("b"),
                &["src2"],
            ),
        ];
        let warnings = overlap_warnings(&views);
        assert!(!has_message(&warnings, "overlapping scope"));
    }

    #[test]
    fn empty_worktrees_are_not_grouped_as_shared() {
        // Old Layer 1 markers default `worktree` to ""; they must not be reported
        // as sharing a worktree just because both are blank.
        let mut a = view("1-1", "claude", "feat/x", Liveness::Active, Some("a"), &[]);
        let mut b = view("2-2", "codex", "feat/y", Liveness::Active, Some("b"), &[]);
        a.marker.worktree = String::new();
        b.marker.worktree = String::new();
        let warnings = overlap_warnings(&[a, b]);
        assert!(!has_message(&warnings, "share worktree"));
    }

    #[test]
    fn active_sessions_are_never_prunable() {
        let m = marker("a", SessionStatus::Active, 0);
        // Even at an absurd age, Active means hands off.
        assert!(!prunable(Liveness::Active, &m, true, u64::MAX, 0));
    }

    #[test]
    fn stale_by_ttl_with_a_live_pid_is_never_prunable() {
        // The long-interactive-session case: open past the TTL (reported
        // stale) but the process is alive and mid-work. Deleting its directory
        // would pull the live packet out from under a running watcher — never,
        // at any age.
        let m = marker("a", SessionStatus::Active, 1_000);
        assert!(!prunable(Liveness::Stale, &m, true, u64::MAX, 0));
    }

    #[test]
    fn closed_session_is_prunable_only_past_retention() {
        let mut m = marker("a", SessionStatus::Closed, 1_000);
        m.closed_at = Some(2_000);
        let retention = DEFAULT_RETENTION_SECS;
        assert!(!prunable(
            Liveness::Closed,
            &m,
            false,
            2_000 + retention,
            retention
        ));
        assert!(prunable(
            Liveness::Closed,
            &m,
            false,
            2_000 + retention + 1,
            retention
        ));
    }

    #[test]
    fn closed_session_ages_out_even_if_its_pid_was_recycled() {
        // Closed is an explicit end-of-life record; a recycled pid that
        // happens to be alive must not keep the directory forever.
        let mut m = marker("a", SessionStatus::Closed, 1_000);
        m.closed_at = Some(2_000);
        assert!(prunable(
            Liveness::Closed,
            &m,
            true,
            2_000 + DEFAULT_RETENTION_SECS + 1,
            DEFAULT_RETENTION_SECS
        ));
    }

    #[test]
    fn closed_session_without_closed_at_ages_from_start() {
        // Old markers predate `closed_at`; fall back to started_at.
        let m = marker("a", SessionStatus::Closed, 1_000);
        assert!(prunable(
            Liveness::Closed,
            &m,
            false,
            1_000 + DEFAULT_RETENTION_SECS + 1,
            DEFAULT_RETENTION_SECS
        ));
    }

    #[test]
    fn stale_session_with_a_dead_pid_ages_from_start() {
        let m = marker("a", SessionStatus::Active, 1_000);
        let retention = DEFAULT_RETENTION_SECS;
        assert!(!prunable(
            Liveness::Stale,
            &m,
            false,
            1_000 + retention,
            retention
        ));
        assert!(prunable(
            Liveness::Stale,
            &m,
            false,
            1_000 + retention + 1,
            retention
        ));
    }

    #[test]
    fn stale_but_open_session_is_flagged() {
        let views = [view(
            "1-1",
            "codex",
            "feat/x",
            Liveness::Stale,
            Some("a"),
            &[],
        )];
        let warnings = overlap_warnings(&views);
        assert!(has_message(&warnings, "marked open but looks stale"));
    }
}
