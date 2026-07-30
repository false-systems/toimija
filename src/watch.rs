//! `toimija watch` — keep the live packet fresh while you work.
//!
//! A foreground command (NOT a daemon): it watches the repo for filesystem and
//! Git-state changes and re-runs a [`refresh::Plan`](crate::refresh::Plan) after
//! a debounce window, until it is stopped (Ctrl-C on the process, or the `stop`
//! flag that `run` drives). No network, no validation execution. Events under
//! build/vendor directories and Toimija's own live output are ignored, so writing
//! the live packet — `.toimija/current.*`, or a per-session
//! `.toimija/sessions/<id>/current.*` under `run` — never re-triggers a refresh.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use notify::{Config, Event, RecursiveMode, Watcher};
use tracing::info;

use crate::command::CommandRunner;
use crate::{consistency, refresh};

/// Coalesce a burst of events for this long before refreshing once.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the wait loop wakes to re-check the stop flag.
const TICK: Duration = Duration::from_millis(200);
/// A wall/monotonic disagreement this large is suspension, not scheduler jitter.
const SUSPENSION_THRESHOLD: Duration = Duration::from_secs(30);
/// How long a watcher has to report its readiness probe before fallback/failure.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Directories whose churn must never trigger a refresh.
const IGNORED_DIRS: [&str; 4] = ["target", "_build", "deps", "node_modules"];

/// Both clocks sampled together. Wall time advances during suspension while
/// the monotonic clock does not on macOS and Linux.
struct ClockPair {
    wall: SystemTime,
    mono: Instant,
}

/// Return the time spent suspended when wall time outruns monotonic time by at
/// least `threshold`. Backwards wall-clock adjustments never fire.
fn detect_suspension(prev: &ClockPair, now: &ClockPair, threshold: Duration) -> Option<Duration> {
    let wall_delta = now.wall.duration_since(prev.wall).ok()?;
    let mono_delta = now.mono.checked_duration_since(prev.mono)?;
    let slept = wall_delta.checked_sub(mono_delta)?;
    (slept >= threshold).then_some(slept)
}

/// A probe-proven filesystem watcher: native when available, polling fallback
/// when the native backend does not report readiness. The payloads are never
/// read — they are held so dropping the backend unregisters the OS watch.
#[cfg_attr(all(target_os = "linux", target_arch = "aarch64"), allow(dead_code))]
enum Backend {
    Native(#[allow(dead_code)] notify::RecommendedWatcher),
    Poll(#[allow(dead_code)] notify::PollWatcher),
}

impl Backend {
    fn label(&self) -> &'static str {
        match self {
            Self::Native(_) => "native",
            Self::Poll(_) => "poll fallback",
        }
    }
}

/// An active filesystem watch. The fallible OS setup is done in [`start`], so a
/// caller can report a startup failure *before* doing other work; the blocking
/// refresh loop is [`Session::run`]. Dropping the session stops watching.
pub struct Session {
    // Held only to keep the OS watch alive; dropping it unregisters the watch.
    backend: Backend,
    rx: mpsc::Receiver<notify::Result<Event>>,
}

/// Begin watching `repo_root`. The fallible parts — creating the watcher and
/// registering the path — happen here, synchronously, so a startup failure is
/// reported to the caller rather than swallowed by a background loop.
pub fn start(repo_root: &Path) -> Result<Session> {
    #[cfg(target_os = "linux")]
    {
        // Inotify can prove the readiness probe while still missing the first
        // post-armed create event in CI/containerized Linux environments.
        // Polling is less clever but matches the foreground-only watch contract.
        start_poll(repo_root, prove_ready)
    }
    #[cfg(not(target_os = "linux"))]
    {
        start_with_probe(repo_root, prove_ready)
    }
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
fn start_with_probe(
    repo_root: &Path,
    probe: impl FnMut(
        &Path,
        &mpsc::Receiver<notify::Result<Event>>,
        Option<&notify::PollWatcher>,
        &str,
    ) -> Result<bool>,
) -> Result<Session> {
    let native = arm::<notify::RecommendedWatcher>(repo_root, Config::default());
    start_from_native(native, repo_root, probe)
}

/// Decide the backend from the native arm attempt: native when it armed *and*
/// proved ready, the poll fallback otherwise. A native arm failure (e.g. the
/// inotify watch limit) must not abort startup while polling can still
/// deliver — it degrades, with the failure logged.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn start_from_native(
    native: Result<(
        notify::RecommendedWatcher,
        mpsc::Receiver<notify::Result<Event>>,
    )>,
    repo_root: &Path,
    mut probe: impl FnMut(
        &Path,
        &mpsc::Receiver<notify::Result<Event>>,
        Option<&notify::PollWatcher>,
        &str,
    ) -> Result<bool>,
) -> Result<Session> {
    match native {
        Ok((native, rx)) => {
            if probe(repo_root, &rx, None, "native")? {
                return armed(Backend::Native(native), rx);
            }
            drop(native);
        }
        Err(error) => {
            info!(
                event = "toimija.watch.native_unavailable",
                error = format!("{error:#}")
            );
        }
    }
    start_poll(repo_root, probe)
}

fn start_poll(
    repo_root: &Path,
    mut probe: impl FnMut(
        &Path,
        &mpsc::Receiver<notify::Result<Event>>,
        Option<&notify::PollWatcher>,
        &str,
    ) -> Result<bool>,
) -> Result<Session> {
    let (poll, rx) = arm::<notify::PollWatcher>(repo_root, watcher_config())?;
    let ready = probe(repo_root, &rx, Some(&poll), "poll")?;
    anyhow::ensure!(ready, "filesystem watcher did not report readiness");
    armed(Backend::Poll(poll), rx)
}

fn armed(backend: Backend, rx: mpsc::Receiver<notify::Result<Event>>) -> Result<Session> {
    info!(event = "toimija.watch.armed", backend = backend.label());
    Ok(Session { backend, rx })
}

fn arm<W: Watcher>(
    repo_root: &Path,
    config: Config,
) -> Result<(W, mpsc::Receiver<notify::Result<Event>>)> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = W::new(
        move |res| {
            let _ = tx.send(res);
        },
        config,
    )
    .context("failed to create filesystem watcher")?;
    watcher
        .watch(repo_root, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", repo_root.display()))?;
    Ok((watcher, rx))
}

fn prove_ready(
    repo_root: &Path,
    rx: &mpsc::Receiver<notify::Result<Event>>,
    poll: Option<&notify::PollWatcher>,
    backend: &str,
) -> Result<bool> {
    let probe = repo_root.join(format!(
        "toimija-watch-probe-{}-{backend}",
        std::process::id()
    ));
    fs::write(&probe, b"watch readiness probe")
        .with_context(|| format!("failed to arm watcher at {}", repo_root.display()))?;
    // Native backends may report the symlink-resolved path — FSEvents turns
    // /tmp/x into /private/tmp/x — so the wait must accept the canonical form
    // of the probe too, or native never proves ready on a symlinked checkout.
    let canonical = fs::canonicalize(&probe).unwrap_or_else(|_| probe.clone());
    let result = wait_for_probe(rx, poll, &probe, &canonical, PROBE_TIMEOUT);
    let _ = fs::remove_file(&probe);
    result
}

fn wait_for_probe(
    rx: &mpsc::Receiver<notify::Result<Event>>,
    poll: Option<&notify::PollWatcher>,
    probe: &Path,
    canonical: &Path,
    timeout: Duration,
) -> Result<bool> {
    if let Some(poll) = poll {
        poll.poll()
            .context("failed to request filesystem watcher readiness poll")?;
    }
    // One total deadline. A per-receive timeout would restart on every event,
    // so steady unrelated churn could stall startup past the fallback window.
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        match rx.recv_timeout(remaining) {
            Ok(Ok(event))
                if event
                    .paths
                    .iter()
                    .any(|path| path == probe || path == canonical) =>
            {
                return Ok(true);
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(false),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(false),
        }
    }
}

fn watcher_config() -> Config {
    Config::default().with_poll_interval(Duration::from_millis(500))
}

impl Session {
    pub fn backend_label(&self) -> &'static str {
        self.backend.label()
    }

    /// Refresh once after the OS watcher is registered. This closes the gap
    /// between the caller's baseline refresh and watcher readiness.
    pub fn catch_up(
        &self,
        runner: &impl CommandRunner,
        repo_root: &Path,
        plan: &refresh::Plan,
    ) -> Result<()> {
        plan.run(runner, repo_root).map(|_| ())
    }

    /// Refresh the live packet on every relevant change until `stop` is set. The
    /// `plan` decides what is written (per-session vs. repo-level). `on_refresh`
    /// is called after each refresh (for status logging).
    pub fn run(
        self,
        runner: &impl CommandRunner,
        repo_root: &Path,
        plan: &refresh::Plan,
        stop: &AtomicBool,
        on_refresh: impl FnMut(),
    ) -> Result<()> {
        self.run_with_clocks(runner, repo_root, plan, stop, on_refresh, || ClockPair {
            wall: SystemTime::now(),
            mono: Instant::now(),
        })
    }

    fn run_with_clocks(
        self,
        runner: &impl CommandRunner,
        repo_root: &Path,
        plan: &refresh::Plan,
        stop: &AtomicBool,
        mut on_refresh: impl FnMut(),
        mut clocks: impl FnMut() -> ClockPair,
    ) -> Result<()> {
        let mut previous = clocks();
        while !stop.load(Ordering::Relaxed) {
            let now = clocks();
            if let Some(slept) = detect_suspension(&previous, &now, SUSPENSION_THRESHOLD) {
                info!(
                    event = "toimija.watch.resumed",
                    slept_secs = slept.as_secs()
                );
                refresh_or_warn(runner, repo_root, plan, &mut on_refresh);
                previous = now;
                continue;
            }
            previous = now;
            match self.rx.recv_timeout(TICK) {
                Ok(Ok(event)) if relevant(&event) => {
                    drain_burst(&self.rx);
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    refresh_or_warn(runner, repo_root, plan, &mut on_refresh);
                }
                // A watcher-backend error (dropped or rescan-required events):
                // surface it, since the packet may now be stale, then keep going.
                Ok(Err(error)) => warn_watcher_error(&error),
                // Irrelevant event: keep watching.
                Ok(Ok(_)) => {}
                // Woke to re-check `stop`.
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                // Watcher dropped its sender — nothing more will arrive. Say
                // so: under `run` the agent keeps working against a packet
                // that has silently stopped refreshing.
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    eprintln!(
                        "toimija: file watcher stopped; the live packet will no longer refresh"
                    );
                    break;
                }
            }
        }
        Ok(())
    }
}

/// A transient refresh failure must not kill live updates: warn and keep
/// watching so the packet can recover on the next event or resume.
fn refresh_or_warn(
    runner: &impl CommandRunner,
    repo_root: &Path,
    plan: &refresh::Plan,
    on_refresh: &mut impl FnMut(),
) {
    match plan.run(runner, repo_root) {
        Ok(_) => on_refresh(),
        Err(error) => eprintln!("toimija: refresh failed (packet may be stale): {error:#}"),
    }
}

/// Convenience for the standalone `watch` command: baseline refresh, then start
/// and catch up after registration before running the loop. Startup failures
/// propagate via `?`.
pub fn watch(
    runner: &impl CommandRunner,
    repo_root: &Path,
    plan: &refresh::Plan,
    stop: &AtomicBool,
    on_refresh: impl FnMut(),
) -> Result<()> {
    // Baseline so the live packet exists immediately, before any event.
    plan.run(runner, repo_root)?;
    let session = start(repo_root)?;
    session.catch_up(runner, repo_root, plan)?;
    eprintln!("toimija watch: armed ({})", session.backend_label());
    session.run(runner, repo_root, plan, stop, on_refresh)
}

/// Swallow the rest of an event burst: keep receiving until the channel is quiet
/// for a full debounce window, so one logical change yields one refresh. Backend
/// errors inside the burst keep their warning — rescan-required is precisely
/// the error that means events were lost.
fn drain_burst(rx: &mpsc::Receiver<notify::Result<Event>>) {
    while let Ok(result) = rx.recv_timeout(DEBOUNCE) {
        if let Err(error) = result {
            warn_watcher_error(&error);
        }
    }
}

/// Warn about a backend error unless it is scoped entirely to ignored
/// directories: a build deleting files under `_build`/`target` mid-scan is
/// churn the watch has promised not to react to, not a packet-freshness
/// problem. A pathless error says nothing about where it happened, so it
/// stays visible.
fn warn_watcher_error(error: &notify::Error) {
    if !error_is_ignorable(error) {
        eprintln!("toimija: file watcher error (packet may be stale): {error}");
    }
}

fn error_is_ignorable(error: &notify::Error) -> bool {
    if error.paths.is_empty() {
        return false;
    }
    if error.paths.iter().all(|path| is_ignored(path)) {
        return true;
    }
    matches!(
        &error.kind,
        notify::ErrorKind::Io(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

fn relevant(event: &Event) -> bool {
    event.paths.iter().any(|path| !is_ignored(path))
}

/// Ignore generated local tool output and anything under a build/vendor
/// directory. Matched on path tails/components so it is robust to the absolute,
/// possibly symlink-resolved paths notify reports.
fn is_ignored(path: &Path) -> bool {
    if is_generated_output(path) {
        return true;
    }
    path.components().any(|component| {
        matches!(component, Component::Normal(name) if IGNORED_DIRS.iter().any(|dir| name == *dir))
    })
}

fn is_generated_output(path: &Path) -> bool {
    let parts: Vec<_> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    parts
        .iter()
        .position(|part| matches!(*part, ".toimija" | ".teko"))
        .is_some_and(|index| consistency::is_generated_output(&PathBuf::from_iter(&parts[index..])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandOutput;
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    struct RefreshCountingRunner {
        refreshes: Cell<usize>,
    }

    struct FailingRunner {
        attempts: Cell<usize>,
    }

    impl CommandRunner for RefreshCountingRunner {
        fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            let (status, stdout) = match (program, args) {
                ("git", ["rev-parse", "--show-toplevel"]) => {
                    (0, cwd.as_os_str().as_encoded_bytes().to_vec())
                }
                ("git", ["--no-optional-locks", "status", ..]) => {
                    // A plan run collects Git status once, so this is the
                    // refresh-count proxy asserted below.
                    self.refreshes.set(self.refreshes.get() + 1);
                    (0, Vec::new())
                }
                _ => (1, Vec::new()),
            };
            Ok(CommandOutput {
                status,
                stdout,
                stderr: Vec::new(),
            })
        }
    }

    impl CommandRunner for FailingRunner {
        fn run(
            &self,
            _cwd: &Path,
            _program: &str,
            _args: &[&str],
        ) -> std::io::Result<CommandOutput> {
            self.attempts.set(self.attempts.get() + 1);
            Err(std::io::Error::other("fabricated refresh failure"))
        }
    }

    fn idle_session() -> Session {
        let watcher = notify::PollWatcher::new(|_| {}, watcher_config()).unwrap();
        let (tx, rx) = mpsc::channel();
        // The dropped sender makes the loop exit on its first quiet tick.
        drop(tx);
        Session {
            backend: Backend::Poll(watcher),
            rx,
        }
    }

    #[test]
    fn suspension_is_detected_when_wall_time_outruns_the_monotonic_clock() {
        let mono = Instant::now();
        let prev = ClockPair {
            wall: UNIX_EPOCH,
            mono,
        };
        let now = ClockPair {
            wall: UNIX_EPOCH + Duration::from_secs(61),
            mono: mono + Duration::from_secs(1),
        };

        assert_eq!(
            detect_suspension(&prev, &now, SUSPENSION_THRESHOLD),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn scheduler_jitter_under_the_threshold_is_not_a_suspension() {
        let mono = Instant::now();
        let prev = ClockPair {
            wall: UNIX_EPOCH,
            mono,
        };
        let now = ClockPair {
            wall: UNIX_EPOCH + Duration::from_secs(30),
            mono: mono + Duration::from_secs(1),
        };

        assert_eq!(detect_suspension(&prev, &now, SUSPENSION_THRESHOLD), None);
    }

    #[test]
    fn a_forward_wall_clock_step_over_threshold_is_a_suspension() {
        let mono = Instant::now();
        let prev = ClockPair {
            wall: UNIX_EPOCH,
            mono,
        };
        let now = ClockPair {
            wall: UNIX_EPOCH + Duration::from_secs(31),
            mono,
        };

        assert_eq!(
            detect_suspension(&prev, &now, SUSPENSION_THRESHOLD),
            Some(Duration::from_secs(31))
        );
    }

    #[test]
    fn a_wall_clock_set_backwards_never_fires() {
        let mono = Instant::now();
        let prev = ClockPair {
            wall: UNIX_EPOCH + Duration::from_secs(60),
            mono,
        };
        let now = ClockPair {
            wall: UNIX_EPOCH,
            mono: mono + Duration::from_secs(60),
        };

        assert_eq!(detect_suspension(&prev, &now, SUSPENSION_THRESHOLD), None);
    }

    #[test]
    fn resume_triggers_exactly_one_refresh_and_heartbeat() {
        let temp = tempfile::tempdir().unwrap();
        let runner = RefreshCountingRunner {
            refreshes: Cell::new(0),
        };
        let plan = refresh::Plan {
            for_diff: false,
            budget: None,
            session_dir: None,
            work: refresh::WorkSource::Discover,
        };
        let stop = AtomicBool::new(false);
        let mono = Instant::now();
        let mut clocks = [
            ClockPair {
                wall: UNIX_EPOCH,
                mono,
            },
            ClockPair {
                wall: UNIX_EPOCH + Duration::from_secs(61),
                mono: mono + Duration::from_secs(1),
            },
        ]
        .into_iter();
        let mut heartbeats = 0;

        idle_session()
            .run_with_clocks(
                &runner,
                temp.path(),
                &plan,
                &stop,
                || {
                    heartbeats += 1;
                    stop.store(true, Ordering::Relaxed);
                },
                || {
                    clocks
                        .next()
                        .expect("loop ran more ticks than the fabricated schedule")
                },
            )
            .unwrap();

        assert_eq!(runner.refreshes.get(), 1);
        assert_eq!(heartbeats, 1);
    }

    #[test]
    fn jitter_under_threshold_triggers_no_refresh() {
        let temp = tempfile::tempdir().unwrap();
        let runner = RefreshCountingRunner {
            refreshes: Cell::new(0),
        };
        let plan = refresh::Plan {
            for_diff: false,
            budget: None,
            session_dir: None,
            work: refresh::WorkSource::Discover,
        };
        let stop = AtomicBool::new(false);
        let mono = Instant::now();
        let mut clocks = [
            ClockPair {
                wall: UNIX_EPOCH,
                mono,
            },
            ClockPair {
                wall: UNIX_EPOCH + Duration::from_secs(30),
                mono: mono + Duration::from_secs(1),
            },
        ]
        .into_iter();
        let mut heartbeats = 0;

        idle_session()
            .run_with_clocks(
                &runner,
                temp.path(),
                &plan,
                &stop,
                || heartbeats += 1,
                || {
                    clocks
                        .next()
                        .expect("loop ran more ticks than the fabricated schedule")
                },
            )
            .unwrap();

        assert_eq!(runner.refreshes.get(), 0);
        assert_eq!(heartbeats, 0);
    }

    #[test]
    fn a_failed_resume_refresh_keeps_watching() {
        let temp = tempfile::tempdir().unwrap();
        let runner = FailingRunner {
            attempts: Cell::new(0),
        };
        let plan = refresh::Plan {
            for_diff: false,
            budget: None,
            session_dir: None,
            work: refresh::WorkSource::Discover,
        };
        let stop = AtomicBool::new(false);
        let mono = Instant::now();
        let mut clocks = [
            ClockPair {
                wall: UNIX_EPOCH,
                mono,
            },
            ClockPair {
                wall: UNIX_EPOCH + Duration::from_secs(61),
                mono: mono + Duration::from_secs(1),
            },
            ClockPair {
                wall: UNIX_EPOCH + Duration::from_secs(61),
                mono: mono + Duration::from_secs(1),
            },
        ]
        .into_iter();
        let mut heartbeats = 0;

        idle_session()
            .run_with_clocks(
                &runner,
                temp.path(),
                &plan,
                &stop,
                || heartbeats += 1,
                || {
                    clocks
                        .next()
                        .expect("loop ran more ticks than the fabricated schedule")
                },
            )
            .unwrap();

        assert_eq!(runner.attempts.get(), 1);
        assert_eq!(heartbeats, 0);
    }

    #[test]
    fn live_files_are_ignored() {
        assert!(is_ignored(&PathBuf::from("/repo/.toimija/current.md")));
        assert!(is_ignored(&PathBuf::from("/repo/.toimija/current.json")));
        assert!(is_ignored(&PathBuf::from("/repo/.teko/teko.redb")));
        // repo.yaml under .toimija is NOT a live file — changes to it matter.
        assert!(!is_ignored(&PathBuf::from("/repo/.toimija/repo.yaml")));
    }

    #[test]
    fn per_session_output_is_ignored_but_repo_yaml_is_not() {
        // The watcher writes per-session packets; those writes must not re-trigger.
        assert!(is_ignored(&PathBuf::from(
            "/repo/.toimija/sessions/1782608449-66987/current.md"
        )));
        assert!(is_ignored(&PathBuf::from(
            "/repo/.toimija/sessions/1782608449-66987/session.json"
        )));
        // Config under .toimija still matters and must NOT be ignored.
        assert!(!is_ignored(&PathBuf::from("/repo/.toimija/repo.yaml")));
    }

    #[test]
    fn runs_output_is_ignored_but_repo_yaml_is_not() {
        assert!(is_ignored(&PathBuf::from(
            "/repo/.toimija/runs/kisko.jsonl"
        )));
        assert!(!is_ignored(&PathBuf::from("/repo/.toimija/repo.yaml")));
    }

    #[test]
    fn build_and_vendor_dirs_are_ignored() {
        assert!(is_ignored(&PathBuf::from("/repo/target/debug/foo")));
        assert!(is_ignored(&PathBuf::from(
            "/repo/apps/web/node_modules/x.js"
        )));
        assert!(is_ignored(&PathBuf::from("/repo/_build/dev/app.beam")));
    }

    #[test]
    fn source_and_git_state_are_relevant() {
        // Source edits and Git-state changes should trigger a refresh.
        assert!(!is_ignored(&PathBuf::from("/repo/src/main.rs")));
        assert!(!is_ignored(&PathBuf::from("/repo/.git/HEAD")));
        assert!(!is_ignored(&PathBuf::from("/repo/.git/index")));
    }

    #[test]
    fn start_surfaces_a_failure_for_an_unwatchable_path() {
        // A non-existent path cannot be watched; `start` must report it rather
        // than a background loop swallowing it.
        let err = match start(&PathBuf::from("/does/not/exist/toimija-watch")) {
            Ok(_) => panic!("start should fail for an unwatchable path"),
            Err(error) => error,
        };
        assert!(format!("{err:#}").contains("watch"));
    }

    #[test]
    fn native_probe_timeout_arms_the_poll_fallback_and_says_so() {
        let temp = tempfile::tempdir().unwrap();
        let session = start_with_probe(temp.path(), |repo_root, rx, poll, backend| {
            if poll.is_none() {
                Ok(false)
            } else {
                prove_ready(repo_root, rx, poll, backend)
            }
        })
        .unwrap();

        assert_eq!(session.backend_label(), "poll fallback");
    }

    #[test]
    fn native_arm_failure_arms_the_poll_fallback_instead_of_aborting() {
        // The inotify watch limit (ENOSPC) fails the native *arm*, not the
        // probe. Startup must degrade to polling, not abort.
        let temp = tempfile::tempdir().unwrap();
        let session = start_from_native(
            Err(anyhow::anyhow!("injected: inotify watch limit reached")),
            temp.path(),
            prove_ready,
        )
        .unwrap();

        assert_eq!(session.backend_label(), "poll fallback");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_uses_the_reliable_poll_backend() {
        let temp = tempfile::tempdir().unwrap();

        let session = start(temp.path()).unwrap();

        assert_eq!(session.backend_label(), "poll fallback");
    }

    #[cfg(unix)]
    #[test]
    fn probe_accepts_the_symlink_resolved_path_native_backends_report() {
        // FSEvents reports /private/tmp/... for a repo rooted at /tmp/...;
        // the probe must prove ready from the canonical form, or native is
        // silently unreachable on any symlinked checkout path.
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let resolved = fs::canonicalize(&real)
            .unwrap()
            .join(format!("toimija-watch-probe-{}-native", std::process::id()));
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(Event::new(notify::EventKind::Create(
            notify::event::CreateKind::File,
        ))
        .add_path(resolved)))
            .unwrap();

        let ready = prove_ready(&link, &rx, None, "native").unwrap();

        assert!(ready);
    }

    #[test]
    fn probe_wait_keeps_one_total_deadline_under_steady_unrelated_churn() {
        // A per-receive timeout would restart on every event; unrelated churn
        // must not stall startup past the fallback window.
        let (tx, rx) = mpsc::channel();
        let feeder = std::thread::spawn(move || {
            for _ in 0..15 {
                let _ = tx.send(Ok(Event::new(notify::EventKind::Modify(
                    notify::event::ModifyKind::Any,
                ))
                .add_path(PathBuf::from("/unrelated"))));
                std::thread::sleep(Duration::from_millis(30));
            }
        });

        let started = Instant::now();
        let ready = wait_for_probe(
            &rx,
            None,
            Path::new("/probe"),
            Path::new("/probe"),
            Duration::from_millis(100),
        )
        .unwrap();
        let elapsed = started.elapsed();
        feeder.join().unwrap();

        assert!(!ready);
        assert!(
            elapsed < Duration::from_millis(300),
            "deadline extended by unrelated events: {elapsed:?}"
        );
    }

    #[test]
    fn errors_scoped_to_ignored_dirs_stay_quiet_but_others_surface() {
        let build_churn = notify::Error::generic("walkdir error scanning")
            .add_path(PathBuf::from("/repo/_build/lib/vartio_core/priv"));
        assert!(error_is_ignorable(&build_churn));

        // A pathless error says nothing about where it happened.
        let pathless = notify::Error::generic("backend failure");
        assert!(!error_is_ignorable(&pathless));

        // One relevant path makes the whole error relevant.
        let mixed = notify::Error::generic("walkdir error scanning")
            .add_path(PathBuf::from("/repo/target/debug/x"))
            .add_path(PathBuf::from("/repo/src/main.rs"));
        assert!(!error_is_ignorable(&mixed));
    }

    #[test]
    fn poll_scan_not_found_races_stay_quiet_but_real_io_errors_surface() {
        let vanished_during_scan =
            notify::Error::io(std::io::Error::from(std::io::ErrorKind::NotFound))
                .add_path(PathBuf::from("/repo/.git/objects/tmp_obj_abcd"));
        assert!(error_is_ignorable(&vanished_during_scan));

        let pathless_not_found =
            notify::Error::io(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert!(!error_is_ignorable(&pathless_not_found));

        let permission_denied =
            notify::Error::io(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
                .add_path(PathBuf::from("/repo/private"));
        assert!(!error_is_ignorable(&permission_denied));
    }
}
