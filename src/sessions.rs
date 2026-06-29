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
use crate::session::{SESSIONS_SUBDIR, SessionMarker, SessionStatus};

/// A session open longer than this with no live process is treated as stale.
/// 90 minutes (decision: start simple; config can come later, not a project).
pub const DEFAULT_STALE_AFTER_SECS: u64 = 90 * 60;

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
    for marker in read_markers(repo_root)? {
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
        sessions.push(SessionView {
            marker,
            liveness,
            packet,
        });
    }
    // Stable order: newest first by start time, then id.
    sessions.sort_by(|a, b| {
        b.marker
            .started_at
            .cmp(&a.marker.started_at)
            .then_with(|| a.marker.id.cmp(&b.marker.id))
    });

    let warnings = overlap_warnings(&sessions);
    info!(
        event = "toimija.sessions.listed",
        total = sessions.len(),
        warnings = warnings.len()
    );
    if !warnings.is_empty() {
        info!(
            event = "toimija.sessions.overlap_detected",
            count = warnings.len()
        );
    }
    Ok(SessionsReport { sessions, warnings })
}

/// Read and parse all `session.json` markers. Missing directory → empty list.
fn read_markers(repo_root: &Path) -> Result<Vec<SessionMarker>> {
    let dir = repo_root.join(SESSIONS_SUBDIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", dir.display()));
        }
    };
    let mut markers = Vec::new();
    for entry in entries {
        let entry = entry?;
        let marker_path = entry.path().join("session.json");
        let raw = match fs::read_to_string(&marker_path) {
            Ok(raw) => raw,
            Err(_) => continue, // no marker in this dir; skip quietly
        };
        match serde_json::from_str::<SessionMarker>(&raw) {
            Ok(marker) => markers.push(marker),
            Err(error) => debug!(
                event = "toimija.session.marker_unreadable",
                path = %marker_path.display(),
                error = %error
            ),
        }
    }
    Ok(markers)
}

/// Resolve liveness from the marker, a process-liveness flag, and the staleness
/// threshold. Pure, so the staleness/active/closed transitions are unit-tested
/// without touching the OS or the clock.
pub fn liveness(
    marker: &SessionMarker,
    now_secs: u64,
    pid_alive: bool,
    stale_after_secs: u64,
) -> Liveness {
    match marker.status {
        SessionStatus::Closed => Liveness::Closed,
        SessionStatus::Active => {
            let age = now_secs.saturating_sub(marker.started_at);
            if !pid_alive || age > stale_after_secs {
                Liveness::Stale
            } else {
                Liveness::Active
            }
        }
    }
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
                "{} is marked open but looks stale (its process is gone or it has been open too long)",
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
            out.push_str(&format!("    packet: {}\n", view.packet));
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
            status,
            closed_at: None,
            exit_status: None,
        }
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
                status: match liveness {
                    Liveness::Closed => SessionStatus::Closed,
                    _ => SessionStatus::Active,
                },
                closed_at: None,
                exit_status: None,
            },
            liveness,
            packet: format!(".toimija/sessions/{id}/current.md"),
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
