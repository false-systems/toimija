//! `toimija sessions` — read-only awareness of sibling sessions in this repo.
//!
//! Presence answers: who is here?  Intent answers: why are they here?
//! Scope answers: what might they touch?  Overlap answers: should another agent
//! be careful?
//!
//! Listing only *reads* `.toimija/sessions/*/session.json`, checks
//! best-effort process liveness, and renders. It assigns nothing, signals
//! nothing, kills nothing, and touches no network. Toimija coordinates
//! visibility, not decisions; the human remains the orchestrator.
//!
//! A marker lives as long as its `run`, which removes it when its agent
//! exits. A `run` killed before it could do so leaves a marker whose process
//! is provably gone: that session is over, so it is not listed, and the next
//! `run` beside it removes it ([`prune_ended`]).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;
use tracing::{debug, info};

use crate::command::{CommandOutput, CommandRunner};
use crate::coverage::Coverage;
use crate::session::{SESSIONS_SUBDIR, SessionMarker};

/// A session whose `run` has not stamped its marker for this long is stale:
/// its process is not provably gone, but it has not been seen. `run` stamps
/// once a minute while its agent runs, so a stale session's `run` is stopped,
/// or runs where this machine cannot see it.
pub const STALE_AFTER_SECS: u64 = 90 * 60;

/// What a `kill -0` probe actually proved. Three-valued because a
/// nonzero exit alone proves nothing: `kill -0` fails with EPERM when the
/// process *exists* under another user, and the probe itself can fail to
/// spawn. Only ESRCH ("no such process") proves death.
///
/// The asymmetry is deliberate and one-directional: a session is left out of
/// the listing, and its marker removed, only on `Dead` — "not provably
/// alive" is never "provably dead".
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

/// Resolved liveness of a listed session, derived from its marker — never
/// stored, always recomputed when listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// Its `run` stamped the marker within [`STALE_AFTER_SECS`].
    Active,
    /// Its process is not provably gone, but its `run` has not stamped the
    /// marker for [`STALE_AFTER_SECS`].
    Stale,
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

/// A session as presented by `toimija sessions`: its marker and resolved
/// liveness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionView {
    #[serde(flatten)]
    pub marker: SessionMarker,
    pub liveness: Liveness,
    /// The process probe, distinct from heartbeat age. Never persisted in the
    /// marker. A listed session is never `dead`.
    pub pid_check: PidCheck,
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

/// Read every session marker under `.toimija/sessions/`, leave out those
/// whose process is provably gone, resolve liveness, and compute overlap
/// warnings. Read-only; a single malformed marker is reported, not fatal —
/// one bad file must not blind an agent to its siblings.
pub fn report(runner: &dyn CommandRunner, root: &Path) -> Result<SessionsReport> {
    let now = now_secs();
    let mut sessions = Vec::new();
    let scan = read_markers(root)?;
    for (_dir, marker) in scan.markers {
        let pid_check = check_marker(runner, root, &marker);
        debug!(
            event = "toimija.session.liveness_checked",
            id = %marker.id,
            pid = marker.pid,
            pid_check = ?pid_check
        );
        if pid_check == PidCheck::Dead {
            continue;
        }
        let liveness = liveness(&marker, now);
        sessions.push(SessionView {
            marker,
            liveness,
            pid_check,
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
    Ok(SessionsReport {
        sessions,
        warnings,
        coverage,
    })
}

/// Remove the session directories under `root` whose marker's process is
/// provably gone: sessions whose `run` was killed before it could remove its
/// own marker. A marker that cannot be read, or whose probe gives no certain
/// answer, is kept: removal demands proof. This process's own marker is never
/// probed. Returns how many were removed.
pub fn prune_ended(runner: &dyn CommandRunner, root: &Path) -> Result<usize> {
    let mut removed = 0;
    for (dir, marker) in read_markers(root)?.markers {
        if marker.pid == std::process::id() {
            continue;
        }
        if check_marker(runner, root, &marker) != PidCheck::Dead {
            continue;
        }
        crate::owned::remove_dir(root, &dir)?;
        removed += 1;
        info!(event = "toimija.session.pruned", session = %marker.id, pid = marker.pid);
    }
    Ok(removed)
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
fn read_markers(root: &Path) -> Result<MarkerScan> {
    let dir = root.join(SESSIONS_SUBDIR);
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

/// Resolve the liveness of a session whose process is not provably gone,
/// from when its `run` last stamped the marker. Pure, so the transition is
/// unit-tested without touching the OS or the clock. A marker with no stamp
/// falls back to `started_at`.
pub fn liveness(marker: &SessionMarker, now_secs: u64) -> Liveness {
    let last_seen = marker.last_seen_at.unwrap_or(marker.started_at);
    if now_secs.saturating_sub(last_seen) > STALE_AFTER_SECS {
        Liveness::Stale
    } else {
        Liveness::Active
    }
}

/// Whether the session marker's process is provably gone: a `run` killed
/// before it could remove its marker. This process's own marker is never
/// probed. Only `Dead` counts; an unanswered probe proves nothing.
pub fn process_gone(cwd: &Path, marker: &SessionMarker) -> bool {
    marker.pid != std::process::id()
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

    // Sessions whose process is not provably gone, but whose `run` has not
    // stamped the marker within the window.
    for view in sessions
        .iter()
        .filter(|view| view.liveness == Liveness::Stale)
    {
        warnings.push(Warning {
            severity: Severity::Warning,
            message: format!(
                "{} is open but has not been seen for 90 minutes (its `toimija run` is stopped, or runs where this machine cannot see it)",
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
/// first, then stale, then a `WARNINGS` block. No session → a single honest
/// line so the agent knows it was answered, not skipped.
pub fn render_text(report: &SessionsReport) -> String {
    let mut out = String::new();
    if report.sessions.is_empty() {
        out.push_str("No open sessions under .toimija/sessions/.\n");
    }
    let now = now_secs();
    for (heading, want) in [("LIVE", Liveness::Active), ("STALE", Liveness::Stale)] {
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

    fn marker(id: &str, started_at: u64) -> SessionMarker {
        SessionMarker {
            id: id.to_string(),
            pid: 4242,
            pid_start: None,
            agent: vec!["codex".to_string()],
            worktree: "/repo".to_string(),
            branch: Some("feat/x".to_string()),
            head: None,
            intent: None,
            scope: Vec::new(),
            started_at,
            last_seen_at: None,
            undelivered: Vec::new(),
        }
    }

    /// Answers every `kill -0` with one exit status and stderr.
    struct ProbeRunner(i32, &'static str);

    impl CommandRunner for ProbeRunner {
        fn run(&self, _cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
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

    fn write_marker(root: &Path, marker: &SessionMarker) -> PathBuf {
        let dir = root.join(SESSIONS_SUBDIR).join(&marker.id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("session.json"),
            serde_json::to_string(marker).unwrap(),
        )
        .unwrap();
        dir
    }

    #[test]
    fn marker_scan_reports_malformed_markers_instead_of_skipping_them() {
        let temp = tempfile::tempdir().unwrap();
        let sessions_dir = temp.path().join(SESSIONS_SUBDIR);
        let good = sessions_dir.join("100-1");
        fs::create_dir_all(&good).unwrap();
        fs::write(
            good.join("session.json"),
            serde_json::to_string(&marker("100-1", 100)).unwrap(),
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
    fn report_keeps_process_evidence_and_leaves_out_only_a_provably_ended_session() {
        let temp = tempfile::tempdir().unwrap();
        let mut seen = marker("100-1", 100);
        seen.last_seen_at = Some(now_secs());
        let dir = write_marker(temp.path(), &seen);
        let raw = fs::read_to_string(dir.join("session.json")).unwrap();
        for (status, stderr, expected) in [
            (0, "", Some("alive")),
            (1, "kill: No such process", None),
            (1, "kill: Operation not permitted", Some("alive")),
            (1, "unexpected failure", Some("unknown")),
        ] {
            let report = report(&ProbeRunner(status, stderr), temp.path()).unwrap();
            let json = serde_json::to_value(&report).unwrap();
            match expected {
                Some(pid_check) => {
                    assert_eq!(json["sessions"][0]["liveness"], "active", "{json}");
                    assert_eq!(json["sessions"][0]["pid_check"], pid_check, "{json}");
                }
                // Its process is gone: the session is over, so it is not listed.
                None => assert!(report.sessions.is_empty(), "{json}"),
            }
            // Listing never rewrites or removes a marker.
            assert_eq!(fs::read_to_string(dir.join("session.json")).unwrap(), raw);
        }
    }

    #[test]
    fn prune_removes_only_markers_whose_process_is_provably_gone() {
        let temp = tempfile::tempdir().unwrap();
        let dir = write_marker(temp.path(), &marker("100-1", 100));
        // A directory with no marker, and an unreadable one, are not sessions
        // this can prove ended: they stay.
        let empty = temp.path().join(SESSIONS_SUBDIR).join("100-2");
        fs::create_dir_all(&empty).unwrap();
        let broken = temp.path().join(SESSIONS_SUBDIR).join("100-3");
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join("session.json"), "{not json").unwrap();

        for (status, stderr) in [
            (0, ""),
            (1, "kill: Operation not permitted"),
            (1, "unexpected failure"),
        ] {
            assert_eq!(
                prune_ended(&ProbeRunner(status, stderr), temp.path()).unwrap(),
                0
            );
            assert!(dir.exists(), "{stderr:?} proves nothing");
        }
        assert_eq!(
            prune_ended(&ProbeRunner(1, "kill: No such process"), temp.path()).unwrap(),
            1
        );
        assert!(!dir.exists());
        assert!(empty.exists() && broken.exists());
    }

    #[test]
    fn a_session_seen_within_the_window_is_active_and_after_it_stale() {
        let mut m = marker("a", 1_000);
        // No stamp: the start counts.
        assert_eq!(liveness(&m, 1_000 + STALE_AFTER_SECS), Liveness::Active);
        assert_eq!(liveness(&m, 1_000 + STALE_AFTER_SECS + 1), Liveness::Stale);
        // A session open for days whose `run` keeps stamping stays active.
        let now = 1_000 + 30 * STALE_AFTER_SECS;
        m.last_seen_at = Some(now - 60);
        assert_eq!(liveness(&m, now), Liveness::Active);
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
                scope: scope.iter().map(|s| s.to_string()).collect(),
                started_at: 0,
                last_seen_at: None,
                undelivered: Vec::new(),
            },
            liveness,
            pid_check: PidCheck::Unknown,
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
        assert!(has_message(&warnings, "has not been seen for 90 minutes"));
    }
}
