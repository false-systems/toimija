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

use crate::command::{CommandOutput, CommandRunner};
use crate::coverage::Coverage;
use crate::session::{SESSIONS_SUBDIR, SessionMarker, SessionStatus};

/// A session open longer than this with no live process is treated as stale.
/// 90 minutes (decision: start simple; config can come later, not a project).
pub const DEFAULT_STALE_AFTER_SECS: u64 = 90 * 60;

/// Closed or stale sessions older than this are removed by `prune`. 7 days —
/// long enough that recent history stays visible in listings, short enough that
/// a busy repo does not accumulate session directories without bound.
pub const DEFAULT_RETENTION_SECS: u64 = 7 * 24 * 60 * 60;

/// What a `kill -0 <pid>` probe actually proved. Three-valued because a
/// nonzero exit alone proves nothing: `kill -0` fails with EPERM when the
/// process *exists* under another user, and the probe itself can fail to
/// spawn. Only ESRCH ("no such process") proves death.
///
/// The asymmetry is deliberate and one-directional: display may give
/// `Unknown` the benefit of the doubt, but deletion (`prunable`) demands
/// `Dead` specifically — "not provably alive" is never "provably dead".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PidCheck {
    /// The process exists: `kill -0` succeeded, or failed with EPERM
    /// (it is alive under another user, just not signalable by us).
    Alive,
    /// The process is provably gone: the probe reported ESRCH, or the pid is
    /// held by a process that started at another time than the one the
    /// marker recorded (a reused pid).
    Dead,
    /// The probe gave no certain answer: it failed to spawn, or failed
    /// with unrecognized diagnostics. Never treated as dead.
    Unknown,
}

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
    /// The process probe, distinct from heartbeat age. Never persisted in the marker.
    pub pid_check: PidCheck,
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
    /// Coverage of the marker scan itself: `available` means
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
        let pid_check = check_marker(runner, repo_root, &marker);
        let liveness = liveness(&marker, now, pid_check, stale_after_secs);
        debug!(
            event = "toimija.session.liveness_checked",
            id = %marker.id,
            pid = marker.pid,
            pid_check = ?pid_check,
            liveness = ?liveness
        );
        let packet = packet_path(repo_root, &marker.id);
        sessions.push(SessionView {
            marker,
            liveness,
            pid_check,
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
/// absent — a directory without a `session.json` is not a
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

/// Resolve liveness from the marker, the pid probe's verdict, and the staleness
/// threshold. Pure, so the staleness/active/closed transitions are unit-tested
/// without touching the OS or the clock.
///
/// The window measures **inactivity**, not lifetime: a
/// day-long session whose watcher keeps stamping `last_seen_at` stays active;
/// one whose heartbeat froze goes stale within the window even if a recycled
/// pid looks alive. A marker with no heartbeat falls back to
/// `started_at`.
///
/// Only a *provably* dead pid downgrades an active marker: a sibling session
/// under another user (EPERM → `Alive`) and a probe that gave no answer
/// (`Unknown`) both leave the heartbeat as the deciding signal, so a
/// transient spawn failure cannot mark every session stale at once.
pub fn liveness(
    marker: &SessionMarker,
    now_secs: u64,
    pid: PidCheck,
    stale_after_secs: u64,
) -> Liveness {
    match marker.status {
        SessionStatus::Closed => Liveness::Closed,
        SessionStatus::Active => {
            let last_seen = marker.last_seen_at.unwrap_or(marker.started_at);
            let idle = now_secs.saturating_sub(last_seen);
            if pid == PidCheck::Dead || idle > stale_after_secs {
                Liveness::Stale
            } else {
                Liveness::Active
            }
        }
    }
}

pub(crate) struct ReapCandidate {
    pub dir: PathBuf,
    pub marker: SessionMarker,
    pub liveness: Liveness,
}

pub(crate) struct ReapPlan {
    pub candidates: Vec<ReapCandidate>,
    pub kept: usize,
    /// Markers whose pid probe returned [`PidCheck::Unknown`]. A persistently
    /// failing probe (no `kill` on PATH, a broken runner) silently blocks
    /// stale pruning; the sweep surfaces this count so the stall is a visible
    /// signal, not a quiet accumulation of session directories.
    pub unknown_probes: usize,
}

/// Identify session directories the footprint sweep may safely reclaim.
/// The sweep owns mutation; this module remains the single owner of liveness.
pub(crate) fn reap_plan(
    runner: &dyn CommandRunner,
    repo_root: &Path,
    stale_after_secs: u64,
    retention_secs: u64,
) -> Result<ReapPlan> {
    let now = now_secs();
    let mut candidates = Vec::new();
    // Unreadable markers are never pruned: deletion demands the certain
    // signal, and an unreadable marker gives none. They count as kept.
    let scan = read_markers(repo_root)?;
    let mut kept = scan.unreadable.len();
    let mut unknown_probes = 0;
    for (dir, marker) in scan.markers {
        let pid_check = check_marker(runner, repo_root, &marker);
        if pid_check == PidCheck::Unknown {
            unknown_probes += 1;
        }
        let liveness = liveness(&marker, now, pid_check, stale_after_secs);
        if prunable(liveness, &marker, pid_check, now, retention_secs) {
            candidates.push(ReapCandidate {
                dir,
                marker,
                liveness,
            });
        } else {
            kept += 1;
        }
    }
    candidates.sort_by(|a, b| a.marker.id.cmp(&b.marker.id));
    Ok(ReapPlan {
        candidates,
        kept,
        unknown_probes,
    })
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
/// pid is *provably* gone (`PidCheck::Dead`): an EPERM answer means a live
/// session under another user, and an `Unknown` probe proves nothing — both
/// keep the directory. A pid recycled by another process reads `Dead` when the
/// marker recorded its own process start (`check_marker`); without that record
/// the caveat holds in the safe direction: the recycled pid reads `Alive`, the
/// session is kept, and retention resumes only once that pid exits.
/// `Closed` is an explicit end-of-life record and ages out regardless of the
/// pid verdict, which by then may point at a recycled pid.
pub fn prunable(
    liveness: Liveness,
    marker: &SessionMarker,
    pid: PidCheck,
    now_secs: u64,
    retention_secs: u64,
) -> bool {
    let reference = match liveness {
        Liveness::Active => return false,
        Liveness::Closed => marker.closed_at.unwrap_or(marker.started_at),
        Liveness::Stale if pid != PidCheck::Dead => return false,
        // Retention counts from when the session was last seen alive, not
        // from its birth.
        Liveness::Stale => marker.last_seen_at.unwrap_or(marker.started_at),
    };
    now_secs.saturating_sub(reference) > retention_secs
}

/// Whether the session a packet names is still running, as `verify` asks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedSession {
    /// The marker is open and its process is not provably gone.
    Live,
    /// The marker was closed: the session ended normally.
    Closed,
    /// The marker is still open but its process is provably gone: the session
    /// was killed before it could close.
    Ended,
    /// No readable marker for that id in this repository.
    Unknown,
}

impl NamedSession {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Closed => "closed",
            Self::Ended => "ended",
            Self::Unknown => "unknown",
        }
    }
}

/// Resolve the session `id` that a packet names. Unlike [`liveness`], an idle
/// marker is not judged here: an agent that has not touched a file for a while
/// is still the session it was, and its packet still briefs it. Only a closed
/// marker, a provably dead process, or no marker at all ends it.
pub fn named_session(runner: &dyn CommandRunner, repo_root: &Path, id: &str) -> NamedSession {
    if !crate::session::is_valid_session_id(id) {
        return NamedSession::Unknown;
    }
    let dir = repo_root.join(SESSIONS_SUBDIR).join(id);
    let Ok(marker) = crate::session::load_marker(&dir) else {
        return NamedSession::Unknown;
    };
    if marker.id != id {
        return NamedSession::Unknown;
    }
    match marker.status {
        SessionStatus::Closed => NamedSession::Closed,
        SessionStatus::Active if check_marker(runner, repo_root, &marker) == PidCheck::Dead => {
            NamedSession::Ended
        }
        SessionStatus::Active => NamedSession::Live,
    }
}

/// Whether the session marker's process is provably gone: a `run` killed
/// before it could close its marker. This process's own marker is never
/// probed. Only `Dead` counts; an unanswered probe proves nothing.
pub fn process_gone(cwd: &Path, marker: &SessionMarker) -> bool {
    marker.status == SessionStatus::Active
        && marker.pid != std::process::id()
        && check_marker(&crate::command::ProcessCommandRunner, cwd, marker) == PidCheck::Dead
}

/// Whether a marker's own process is alive: its pid probed with `kill -0`,
/// then, when the marker recorded when that process started, the start of the
/// process holding the pid now. A pid outlives its process and is handed to
/// another; a live pid that started at another time is that other process, so
/// the session's own is provably gone (`Dead`), whatever `kill -0` answered.
fn check_marker(runner: &dyn CommandRunner, cwd: &Path, marker: &SessionMarker) -> PidCheck {
    same_process(check_pid(runner, cwd, marker.pid), marker.pid_start, || {
        process_start(marker.pid)
    })
}

/// The probe's verdict, unless the pid is held by a process that started at
/// another time than the one recorded. Pure, so the decision is unit-tested
/// without real processes. Only two known, different starts prove another
/// process; a start that cannot be read leaves the probe's verdict as it was.
fn same_process(
    probed: PidCheck,
    recorded: Option<u64>,
    current: impl FnOnce() -> Option<u64>,
) -> PidCheck {
    if probed == PidCheck::Dead {
        return probed;
    }
    match (recorded, recorded.and_then(|_| current())) {
        (Some(recorded), Some(now)) if recorded != now => PidCheck::Dead,
        _ => probed,
    }
}

/// When process `pid` started, as the operating system records it, read
/// without starting a process: an opaque token, compared only for equality
/// with one read on the same machine. `None` where it cannot be read (no such
/// process, or a platform without a reader).
///
/// - Linux: `starttime` from `/proc/<pid>/stat`, clock ticks after boot.
/// - macOS: `kp_proc.p_starttime` from `sysctl(KERN_PROC_PID)`, in
///   microseconds since the epoch. The sysctl answers for any user's process.
pub fn process_start(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        proc_stat_start(&stat)
    }
    #[cfg(target_os = "macos")]
    {
        macos_process_start(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// Field 22 of `/proc/<pid>/stat`, `starttime`. Field 2, the command name, is
/// in parentheses and may itself hold spaces and parentheses, so the fields
/// are counted from the last `)`: field 3 is the first after it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn proc_stat_start(stat: &str) -> Option<u64> {
    let after_name = &stat[stat.rfind(')')? + 1..];
    after_name.split_whitespace().nth(22 - 3)?.parse().ok()
}

#[cfg(target_os = "macos")]
fn macos_process_start(pid: u32) -> Option<u64> {
    // `struct kinfo_proc` opens with `kp_proc.p_un`, a union whose
    // `__p_starttime` is a `struct timeval` (`tv_sec: i64`, `tv_usec: i32`).
    // libc does not declare the struct, so the answer is read as bytes; the
    // whole struct is 648 bytes, and an absent pid answers with none.
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        libc::c_int::try_from(pid).ok()?,
    ];
    let mut buffer = [0u8; 1024];
    let mut size = buffer.len();
    // SAFETY: `mib` names four levels; `buffer` is writable for `size` bytes,
    // and the kernel writes at most `size` bytes and stores the count in it.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || size < 16 {
        return None;
    }
    let seconds = i64::from_ne_bytes(buffer[0..8].try_into().ok()?);
    let micros = i32::from_ne_bytes(buffer[8..12].try_into().ok()?);
    u64::try_from(seconds)
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(u64::try_from(micros).ok()?)
}

/// Best-effort process liveness via `kill -0 <pid>`. Routed through the runner
/// so it is mockable in tests. `LC_ALL=C` pins the probe's diagnostics to the
/// untranslated strerror text so classification does not depend on the host
/// locale. Alone, a recycled pid can mislead it — awareness, not authority —
/// which [`check_marker`] corrects where the marker recorded its process start;
/// the answer is honest about what it proved (see [`PidCheck`]).
fn check_pid(runner: &dyn CommandRunner, cwd: &Path, pid: u32) -> PidCheck {
    let pid = pid.to_string();
    let result = runner.run_with_env(
        cwd,
        "kill",
        &["-0", &pid],
        &[("LC_ALL", std::ffi::OsStr::new("C"))],
    );
    classify_kill_probe(result)
}

/// Classify a `kill -0` probe result. Pure, so the ESRCH/EPERM/unknown
/// distinctions are unit-tested without spawning processes.
///
/// Exit 0 proves the process exists. A nonzero exit is ambiguous — `kill`
/// exits 1 for both ESRCH and EPERM — so the verdict comes from the strerror
/// text on stderr: "no such process" (ESRCH) is the only phrase that proves
/// death, "not permitted" (EPERM) proves life under another user, and
/// anything else — including a probe that failed to spawn — is `Unknown`.
fn classify_kill_probe(result: std::io::Result<CommandOutput>) -> PidCheck {
    let output = match result {
        Ok(output) => output,
        Err(_) => return PidCheck::Unknown,
    };
    if output.status == 0 {
        return PidCheck::Alive;
    }
    let stderr = output.stderr_lossy().to_ascii_lowercase();
    if stderr.contains("no such process") {
        PidCheck::Dead
    } else if stderr.contains("not permitted") {
        PidCheck::Alive
    } else {
        PidCheck::Unknown
    }
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
            if let Some(spec) = &m.spec {
                out.push_str(&format!("    spec: {spec}\n"));
            }
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
            pid_start: None,
            agent: vec!["codex".to_string()],
            worktree: "/repo".to_string(),
            branch: Some("feat/x".to_string()),
            head: None,
            intent: None,
            spec: None,
            scope: Vec::new(),
            started_at,
            last_seen_at: None,
            status,
            closed_at: None,
            exit_status: None,

            undelivered: Vec::new(),
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

    fn probe(status: i32, stderr: &str) -> std::io::Result<CommandOutput> {
        Ok(CommandOutput {
            status,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    #[test]
    fn report_preserves_process_evidence_without_rewriting_the_marker() {
        struct ProbeRunner(i32, &'static str);
        impl CommandRunner for ProbeRunner {
            fn run(
                &self,
                _cwd: &Path,
                program: &str,
                args: &[&str],
            ) -> std::io::Result<CommandOutput> {
                assert_eq!(program, "kill");
                assert_eq!(args, ["-0", "4242"]);
                probe(self.0, self.1)
            }
            fn run_with_env(
                &self,
                cwd: &Path,
                program: &str,
                args: &[&str],
                _env: &[(&str, &std::ffi::OsStr)],
            ) -> std::io::Result<CommandOutput> {
                self.run(cwd, program, args)
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(SESSIONS_SUBDIR).join("100-1");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        let raw = serde_json::to_string(&marker("100-1", SessionStatus::Active, 100)).unwrap();
        fs::write(&path, &raw).unwrap();
        for (status, stderr, expected) in [
            (0, "", "alive"),
            (1, "kill: No such process", "dead"),
            (1, "kill: Operation not permitted", "alive"),
            (1, "unexpected failure", "unknown"),
        ] {
            let report = report(
                &ProbeRunner(status, stderr),
                temp.path(),
                DEFAULT_STALE_AFTER_SECS,
            )
            .unwrap();
            let json = serde_json::to_value(report).unwrap();
            assert_eq!(json["sessions"][0]["liveness"], "stale");
            assert_eq!(json["sessions"][0]["pid_check"], expected);
            assert_eq!(fs::read_to_string(&path).unwrap(), raw);
        }
    }

    #[test]
    fn probe_exit_zero_is_alive() {
        assert_eq!(classify_kill_probe(probe(0, "")), PidCheck::Alive);
    }

    #[test]
    fn only_two_known_different_starts_prove_another_process() {
        use PidCheck::{Alive, Dead, Unknown};
        let unread = || -> Option<u64> { panic!("nothing recorded, nothing to read") };
        // No recorded start (an older marker): the probe decides.
        assert_eq!(same_process(Alive, None, unread), Alive);
        // The same start: the session's own process.
        assert_eq!(same_process(Alive, Some(7), || Some(7)), Alive);
        // Another start: the pid was handed on; EPERM and an unanswered
        // probe are overruled too, since the start was read.
        assert_eq!(same_process(Alive, Some(7), || Some(8)), Dead);
        assert_eq!(same_process(Unknown, Some(7), || Some(8)), Dead);
        // A start that cannot be read proves nothing either way.
        assert_eq!(same_process(Alive, Some(7), || None), Alive);
        assert_eq!(same_process(Unknown, Some(7), || None), Unknown);
        // Dead stays dead without reading anything.
        assert_eq!(same_process(Dead, Some(7), unread), Dead);
    }

    #[test]
    fn proc_stat_start_counts_fields_after_the_command_name() {
        // Field 22 is `starttime`; a command name with spaces and parentheses
        // must not shift the count.
        let fields: Vec<String> = (3..=30).map(|field| field.to_string()).collect();
        let stat = format!("4242 (a (b) c) {}\n", fields.join(" "));
        assert_eq!(proc_stat_start(&stat), Some(22));
        assert_eq!(proc_stat_start("4242 (sh) R 1 2"), None);
        assert_eq!(proc_stat_start("no name"), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_process_start_is_stable_and_a_later_process_starts_later() {
        let own = process_start(std::process::id()).expect("own start");
        assert_eq!(process_start(std::process::id()), Some(own));
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let later = process_start(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(later.expect("child start") >= own);
        // A pid above any pid limit has no process.
        assert_eq!(process_start(2_000_000_000), None);
        // On macOS the token is microseconds since the epoch: this process
        // started within the last day, which checks the struct is read right.
        #[cfg(target_os = "macos")]
        {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64;
            assert!(own <= now && now - own < 86_400_000_000, "{own} vs {now}");
        }
    }

    #[test]
    fn probe_pins_the_c_locale() {
        // Classification parses strerror text, so the probe must go through
        // `run_with_env` with LC_ALL=C — a regression to plain `run()` would
        // reintroduce locale-dependent verdicts and pass every other test.
        struct EnvAssertingRunner;

        impl CommandRunner for EnvAssertingRunner {
            fn run(
                &self,
                _cwd: &Path,
                _program: &str,
                _args: &[&str],
            ) -> std::io::Result<CommandOutput> {
                panic!("check_pid must use run_with_env so LC_ALL can be pinned");
            }

            fn run_with_env(
                &self,
                _cwd: &Path,
                program: &str,
                args: &[&str],
                env: &[(&str, &std::ffi::OsStr)],
            ) -> std::io::Result<CommandOutput> {
                assert_eq!(program, "kill");
                assert_eq!(args, ["-0", "4242"]);
                assert!(
                    env.iter()
                        .any(|(key, value)| *key == "LC_ALL" && *value == "C"),
                    "probe must pin LC_ALL=C, got {env:?}"
                );
                Ok(CommandOutput {
                    status: 0,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                })
            }
        }

        assert_eq!(
            check_pid(&EnvAssertingRunner, Path::new("."), 4242),
            PidCheck::Alive
        );
    }

    #[test]
    fn probe_esrch_is_the_only_dead_verdict() {
        // util-linux phrasing.
        assert_eq!(
            classify_kill_probe(probe(
                1,
                "kill: sending signal to 4242 failed: No such process"
            )),
            PidCheck::Dead
        );
        // BSD/macOS phrasing.
        assert_eq!(
            classify_kill_probe(probe(1, "kill: 4242: No such process")),
            PidCheck::Dead
        );
    }

    #[test]
    fn probe_eperm_means_alive_under_another_user() {
        // A sibling session owned by another user answers EPERM.
        // That is proof of existence, not of death.
        assert_eq!(
            classify_kill_probe(probe(1, "kill: 4242: Operation not permitted")),
            PidCheck::Alive
        );
    }

    #[test]
    fn probe_spawn_failure_is_unknown() {
        let error: std::io::Result<CommandOutput> =
            Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(classify_kill_probe(error), PidCheck::Unknown);
    }

    #[test]
    fn probe_unrecognized_diagnostics_are_unknown() {
        // A nonzero exit with no recognizable strerror text proves nothing —
        // fail toward keeping, never toward deleting.
        assert_eq!(classify_kill_probe(probe(1, "")), PidCheck::Unknown);
        assert_eq!(
            classify_kill_probe(probe(1, "kill: something surprising")),
            PidCheck::Unknown
        );
    }

    #[test]
    fn closed_marker_is_always_closed() {
        let m = marker("a", SessionStatus::Closed, 0);
        assert_eq!(
            liveness(&m, 1_000, PidCheck::Alive, DEFAULT_STALE_AFTER_SECS),
            Liveness::Closed
        );
    }

    #[test]
    fn open_marker_with_live_pid_is_active() {
        let m = marker("a", SessionStatus::Active, 1_000);
        assert_eq!(
            liveness(&m, 1_100, PidCheck::Alive, DEFAULT_STALE_AFTER_SECS),
            Liveness::Active
        );
    }

    #[test]
    fn open_marker_with_dead_pid_is_stale() {
        let m = marker("a", SessionStatus::Active, 1_000);
        assert_eq!(
            liveness(&m, 1_100, PidCheck::Dead, DEFAULT_STALE_AFTER_SECS),
            Liveness::Stale
        );
    }

    #[test]
    fn open_marker_past_threshold_is_stale_even_if_pid_alive() {
        // No heartbeat recorded: inactivity falls back to started_at.
        let m = marker("a", SessionStatus::Active, 1_000);
        // now is well past started_at + threshold.
        assert_eq!(
            liveness(
                &m,
                1_000 + DEFAULT_STALE_AFTER_SECS + 1,
                PidCheck::Alive,
                DEFAULT_STALE_AFTER_SECS
            ),
            Liveness::Stale
        );
    }

    #[test]
    fn long_session_with_a_recent_heartbeat_stays_active() {
        // A session open for days whose
        // watcher keeps stamping last_seen_at is LIVE, not stale.
        let mut m = marker("a", SessionStatus::Active, 1_000);
        let now = 1_000 + 30 * DEFAULT_STALE_AFTER_SECS;
        m.last_seen_at = Some(now - 60);
        assert_eq!(
            liveness(&m, now, PidCheck::Alive, DEFAULT_STALE_AFTER_SECS),
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
                PidCheck::Alive,
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
            PidCheck::Dead,
            last_seen + retention,
            retention
        ));
        assert!(prunable(
            Liveness::Stale,
            &m,
            PidCheck::Dead,
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
                pid_start: None,
                agent: vec![agent.to_string()],
                worktree: "/repo".to_string(),
                branch: Some(branch.to_string()),
                head: None,
                intent: intent.map(str::to_string),
                spec: None,
                scope: scope.iter().map(|s| s.to_string()).collect(),
                started_at: 0,
                last_seen_at: None,
                status: match liveness {
                    Liveness::Closed => SessionStatus::Closed,
                    _ => SessionStatus::Active,
                },
                closed_at: None,
                exit_status: None,
                undelivered: Vec::new(),
            },
            liveness,
            pid_check: PidCheck::Unknown,
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
    fn active_sessions_are_never_prunable() {
        let m = marker("a", SessionStatus::Active, 0);
        // Even at an absurd age, Active means hands off.
        assert!(!prunable(
            Liveness::Active,
            &m,
            PidCheck::Alive,
            u64::MAX,
            0
        ));
    }

    #[test]
    fn stale_by_ttl_with_a_live_pid_is_never_prunable() {
        // The long-interactive-session case: open past the TTL (reported
        // stale) but the process is alive and mid-work. Deleting its directory
        // would pull the live packet out from under a running watcher — never,
        // at any age.
        let m = marker("a", SessionStatus::Active, 1_000);
        assert!(!prunable(Liveness::Stale, &m, PidCheck::Alive, u64::MAX, 0));
    }

    #[test]
    fn stale_session_with_an_unknown_pid_is_never_prunable() {
        // A probe that failed to spawn (or answered unintelligibly) proves
        // nothing; `remove_dir_all` demands proof. Even at an absurd age.
        let m = marker("a", SessionStatus::Active, 1_000);
        assert!(!prunable(
            Liveness::Stale,
            &m,
            PidCheck::Unknown,
            u64::MAX,
            0
        ));
    }

    #[test]
    fn unknown_probe_leaves_the_heartbeat_deciding() {
        // A transient spawn failure must not mark every session stale at
        // once: with a fresh heartbeat the session stays active...
        let mut m = marker("a", SessionStatus::Active, 1_000);
        let now = 10_000;
        m.last_seen_at = Some(now - 60);
        assert_eq!(
            liveness(&m, now, PidCheck::Unknown, DEFAULT_STALE_AFTER_SECS),
            Liveness::Active
        );
        // ...while a frozen heartbeat still goes stale on its own.
        m.last_seen_at = Some(1_000);
        assert_eq!(
            liveness(
                &m,
                1_000 + DEFAULT_STALE_AFTER_SECS + 1,
                PidCheck::Unknown,
                DEFAULT_STALE_AFTER_SECS
            ),
            Liveness::Stale
        );
    }

    #[test]
    fn closed_session_is_prunable_only_past_retention() {
        let mut m = marker("a", SessionStatus::Closed, 1_000);
        m.closed_at = Some(2_000);
        let retention = DEFAULT_RETENTION_SECS;
        assert!(!prunable(
            Liveness::Closed,
            &m,
            PidCheck::Dead,
            2_000 + retention,
            retention
        ));
        assert!(prunable(
            Liveness::Closed,
            &m,
            PidCheck::Dead,
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
            PidCheck::Alive,
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
            PidCheck::Dead,
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
            PidCheck::Dead,
            1_000 + retention,
            retention
        ));
        assert!(prunable(
            Liveness::Stale,
            &m,
            PidCheck::Dead,
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
