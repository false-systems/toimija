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
use tracing::{debug, info};

use crate::command::CommandRunner;
use crate::{consistency, git, refresh};

/// Coalesce a burst of events for this long before refreshing once.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the wait loop wakes to re-check the stop flag.
const TICK: Duration = Duration::from_millis(200);
/// A wall/monotonic disagreement this large is suspension, not scheduler jitter.
const SUSPENSION_THRESHOLD: Duration = Duration::from_secs(30);
/// How long a watcher has to report its readiness probe before fallback/failure.
#[cfg_attr(target_os = "linux", allow(dead_code))]
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Polling cadence for packet-visible Git state.
const STATE_POLL_INTERVAL: Duration = Duration::from_millis(500);
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

/// A probe-proven change detector: native when available, notify polling when
/// native does not report readiness, and Git-state polling on Linux. Watcher
/// payloads are held so dropping the backend unregisters the OS watch.
#[cfg_attr(target_os = "linux", allow(dead_code))]
enum Backend {
    Native(#[allow(dead_code)] notify::RecommendedWatcher),
    Poll(#[allow(dead_code)] notify::PollWatcher),
    #[allow(dead_code)]
    GitPoll,
}

impl Backend {
    fn label(&self) -> &'static str {
        match self {
            Self::Native(_) => "native",
            Self::Poll(_) => "poll fallback",
            Self::GitPoll => "git polling",
        }
    }
}

/// An active filesystem watch. The fallible OS setup is done in [`start`], so a
/// caller can report a startup failure *before* doing other work; the blocking
/// refresh loop is [`Session::run`]. Dropping the session stops watching.
pub struct Session {
    // Held only to keep the OS watch alive; dropping it unregisters the watch.
    backend: Backend,
    rx: Option<mpsc::Receiver<notify::Result<Event>>>,
    baseline_token: Option<WatchToken>,
}

/// Begin watching `repo_root`. The fallible parts — creating the watcher and
/// registering the path — happen here, synchronously, so a startup failure is
/// reported to the caller rather than swallowed by a background loop.
pub fn start(repo_root: &Path) -> Result<Session> {
    #[cfg(target_os = "linux")]
    {
        // Inotify can prove the readiness probe while still missing the first
        // post-armed create event in CI/containerized Linux environments.
        // Toimija only needs packet-visible Git state, so avoid recursively
        // walking the whole repo through notify's polling backend.
        fs::metadata(repo_root)
            .with_context(|| format!("failed to watch {}", repo_root.display()))?;
        armed(Backend::GitPoll, None)
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
                return armed(Backend::Native(native), Some(rx));
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
    armed(Backend::Poll(poll), Some(rx))
}

fn armed(backend: Backend, rx: Option<mpsc::Receiver<notify::Result<Event>>>) -> Result<Session> {
    info!(event = "toimija.watch.armed", backend = backend.label());
    Ok(Session {
        backend,
        rx,
        baseline_token: None,
    })
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

#[cfg_attr(target_os = "linux", allow(dead_code))]
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
    // Never through a symlink planted at the probe's name.
    crate::owned::open_nofollow(
        &probe,
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true),
    )
    .and_then(|mut file| std::io::Write::write_all(&mut file, b"watch readiness probe"))
    .with_context(|| format!("failed to arm watcher at {}", repo_root.display()))?;
    // Native backends may report the symlink-resolved path — FSEvents turns
    // /tmp/x into /private/tmp/x — so the wait must accept the canonical form
    // of the probe too, or native never proves ready on a symlinked checkout.
    let canonical = fs::canonicalize(&probe).unwrap_or_else(|_| probe.clone());
    let result = wait_for_probe(rx, poll, &probe, &canonical, PROBE_TIMEOUT);
    let _ = fs::remove_file(&probe);
    result
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
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
        &mut self,
        runner: &impl CommandRunner,
        repo_root: &Path,
        plan: &refresh::Plan,
    ) -> Result<()> {
        let refreshed = plan.run(runner, repo_root)?;
        self.baseline_token = Some(packet_refresh_token(&refreshed.packet));
        Ok(())
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
        if matches!(&self.backend, Backend::GitPoll) {
            return run_git_polling(
                runner,
                repo_root,
                plan,
                stop,
                on_refresh,
                clocks,
                self.baseline_token,
            );
        }
        let rx = self
            .rx
            .as_ref()
            .context("filesystem watcher backend missing event receiver")?;
        let mut previous = clocks();
        let mut previous_token = self.baseline_token;
        let mut last_poll = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let now = clocks();
            if let Some(slept) = detect_suspension(&previous, &now, SUSPENSION_THRESHOLD) {
                info!(
                    event = "toimija.watch.resumed",
                    slept_secs = slept.as_secs()
                );
                if let Some(token) = refresh_or_warn(runner, repo_root, plan, &mut on_refresh) {
                    previous_token = Some(token);
                }
                previous = now;
                continue;
            }
            previous = now;
            match rx.recv_timeout(TICK) {
                Ok(Ok(event)) if relevant(&event) => {
                    drain_burst(rx, repo_root);
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Some(token) = refresh_or_warn(runner, repo_root, plan, &mut on_refresh) {
                        previous_token = Some(token);
                    }
                }
                // A watcher-backend error (dropped or rescan-required events):
                // surface it, since the packet may now be stale, then keep going.
                Ok(Err(error)) => warn_watcher_error(&error, repo_root),
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
            poll_refresh_if_changed(
                runner,
                repo_root,
                plan,
                &mut previous_token,
                &mut last_poll,
                &mut on_refresh,
            );
        }
        Ok(())
    }
}

fn run_git_polling(
    runner: &impl CommandRunner,
    repo_root: &Path,
    plan: &refresh::Plan,
    stop: &AtomicBool,
    mut on_refresh: impl FnMut(),
    mut clocks: impl FnMut() -> ClockPair,
    baseline_token: Option<WatchToken>,
) -> Result<()> {
    let mut previous = clocks();
    let mut previous_token = baseline_token.or_else(|| match current_token(runner, repo_root) {
        Ok(token) => Some(token),
        Err(error) => {
            eprintln!("toimija: Git-state poll failed (packet may be stale): {error:#}");
            None
        }
    });
    let mut last_poll = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let now = clocks();
        if let Some(slept) = detect_suspension(&previous, &now, SUSPENSION_THRESHOLD) {
            info!(
                event = "toimija.watch.resumed",
                slept_secs = slept.as_secs()
            );
            if let Some(token) = refresh_or_warn(runner, repo_root, plan, &mut on_refresh) {
                previous_token = Some(token);
            }
            previous = now;
            continue;
        }
        previous = now;

        poll_refresh_if_changed(
            runner,
            repo_root,
            plan,
            &mut previous_token,
            &mut last_poll,
            &mut on_refresh,
        );

        std::thread::sleep(TICK);
    }
    Ok(())
}

fn poll_refresh_if_changed(
    runner: &impl CommandRunner,
    repo_root: &Path,
    plan: &refresh::Plan,
    previous_token: &mut Option<WatchToken>,
    last_poll: &mut Instant,
    on_refresh: &mut impl FnMut(),
) {
    if last_poll.elapsed() < STATE_POLL_INTERVAL {
        return;
    }
    *last_poll = Instant::now();
    match current_token(runner, repo_root) {
        Ok(current) if previous_token.as_ref() != Some(&current) => {
            if previous_token.is_some() {
                debug!(event = "toimija.watch.state_poll_changed");
            }
            if let Some(token) = refresh_or_warn(runner, repo_root, plan, on_refresh) {
                *previous_token = Some(token);
            }
        }
        Ok(current) => *previous_token = Some(current),
        Err(error) => {
            eprintln!("toimija: Git-state poll failed (packet may be stale): {error:#}");
        }
    }
}

/// What the watcher compares between polls: the packet's repository refresh
/// token (proposal 0011).
type WatchToken = String;

fn current_token(runner: &impl CommandRunner, repo_root: &Path) -> Result<WatchToken> {
    let (mut git, _) = git::collect_git_state(runner, repo_root)?;
    crate::context::normalize_git_state(&mut git);
    Ok(crate::context::refresh_token(&git))
}

/// The packet-side counterpart of [`current_token`]. Equality with the live
/// probe on an untouched repo is pinned by
/// `packet_and_live_watch_tokens_agree_when_nothing_changes`.
fn packet_refresh_token(packet: &crate::context::ContextPacket) -> WatchToken {
    packet
        .refresh_token
        .clone()
        .unwrap_or_else(|| packet.consistency.token.clone())
}

/// A transient refresh failure must not kill live updates: keep watching so
/// the packet can recover on the next event or resume. But the agent reads the
/// packet, not this process's stderr, so the failure goes into the packet
/// itself (issue #229) — once per distinct failure, not on every retry.
fn refresh_or_warn(
    runner: &impl CommandRunner,
    repo_root: &Path,
    plan: &refresh::Plan,
    on_refresh: &mut impl FnMut(),
) -> Option<WatchToken> {
    match plan.run(runner, repo_root) {
        Ok(refreshed) => {
            let token = packet_refresh_token(&refreshed.packet);
            on_refresh();
            Some(token)
        }
        Err(error) => {
            match plan.mark_stale(repo_root, &error) {
                Ok(true) => {
                    eprintln!("toimija: refresh failed; the packet now says it is stale: {error:#}")
                }
                // Already said, in the packet and here: a retry is not news.
                Ok(false) => {}
                Err(mark) => eprintln!(
                    "toimija: refresh failed (packet may be stale): {error:#}; \
                     marking the packet stale also failed: {mark:#}"
                ),
            }
            None
        }
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
    let mut session = start(repo_root)?;
    session.catch_up(runner, repo_root, plan)?;
    eprintln!("toimija watch: armed ({})", session.backend_label());
    session.run(runner, repo_root, plan, stop, on_refresh)
}

/// Swallow the rest of an event burst: keep receiving until the channel is quiet
/// for a full debounce window, so one logical change yields one refresh. Backend
/// errors inside the burst keep their warning — rescan-required is precisely
/// the error that means events were lost.
fn drain_burst(rx: &mpsc::Receiver<notify::Result<Event>>, repo_root: &Path) {
    while let Ok(result) = rx.recv_timeout(DEBOUNCE) {
        if let Err(error) = result {
            warn_watcher_error(&error, repo_root);
        }
    }
}

/// Warn about a backend error unless it is scoped entirely to ignored
/// directories: a build deleting files under `_build`/`target` mid-scan is
/// churn the watch has promised not to react to, not a packet-freshness
/// problem. Polling can also race children that vanish mid-walk; those
/// path-scoped `NotFound` errors are quiet unless the watched root itself is
/// gone. A pathless error says nothing about where it happened, so it stays
/// visible.
fn warn_watcher_error(error: &notify::Error, repo_root: &Path) {
    if !error_is_ignorable(error, repo_root) {
        eprintln!("toimija: file watcher error (packet may be stale): {error}");
    }
}

fn error_is_ignorable(error: &notify::Error, repo_root: &Path) -> bool {
    if error.paths.is_empty() {
        return false;
    }
    if matches!(
        &error.kind,
        notify::ErrorKind::Io(io_error)
            if io_error.kind() == std::io::ErrorKind::NotFound
    ) {
        return repo_root.try_exists().unwrap_or(false);
    }
    error.paths.iter().all(|path| is_ignored(path))
}

fn relevant(event: &Event) -> bool {
    event.paths.iter().any(|path| !is_ignored(path))
}

/// Ignore generated local tool output and anything under a build/vendor
/// directory. Matched on path tails/components so it is robust to the absolute,
/// possibly symlink-resolved paths notify reports.
fn is_ignored(path: &Path) -> bool {
    if is_generated_output(path) || is_toimija_git_footprint(path) {
        return true;
    }
    path.components().any(|component| {
        matches!(component, Component::Normal(name) if IGNORED_DIRS.iter().any(|dir| name == *dir))
    })
}

/// Toimija's own Git writes must not refresh the packet that caused them.
/// Keep HEAD, index, reflogs, and packed refs relevant: they can describe a
/// real repository transition rather than observer bookkeeping.
fn is_toimija_git_footprint(path: &Path) -> bool {
    let parts: Vec<_> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    parts
        .windows(2)
        .any(|pair| pair == [".git", "objects"] || pair == ["refs", "toimija"])
        || parts
            .windows(2)
            .any(|pair| pair[0] == ".git" && pair[1].starts_with("toimija-snapshot-"))
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
            rx: Some(rx),
            baseline_token: None,
        }
    }

    #[test]
    fn packet_and_live_watch_tokens_agree_when_nothing_changes() {
        // D11-5's churn guard: the packet-side token and the live probe must
        // agree on an untouched repo, or the state poll would trigger a
        // refresh every interval forever — invisible to the drift tests,
        // which only assert that a refresh *happens*. Going through the real
        // packet build pins the whole chain.
        // Scrub the redirecting variables: `command::tests` sets `GIT_DIR`
        // process-wide for a moment, and a fixture `git` that inherits it
        // acts on that test's repository instead of this one.
        let git_runs = |args: &[&str], dir: &Path| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env_remove("GIT_DIR")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_WORK_TREE")
                .output()
                .is_ok_and(|output| output.status.success())
        };
        let temp = tempfile::tempdir().unwrap();
        if !git_runs(&["init"], temp.path()) {
            return; // no usable git on this machine; nothing to pin
        }
        assert!(git_runs(
            &["config", "user.email", "toimija@example.invalid"],
            temp.path()
        ));
        assert!(git_runs(
            &["config", "user.name", "Toimija Test"],
            temp.path()
        ));
        std::fs::write(temp.path().join("a.txt"), "x\n").unwrap();
        assert!(git_runs(&["add", "."], temp.path()));
        assert!(git_runs(&["commit", "-m", "init"], temp.path()));

        let runner = crate::command::ProcessCommandRunner;
        let packet = crate::context::build_context_packet(&runner, temp.path(), false).unwrap();
        assert_eq!(
            packet_refresh_token(&packet),
            current_token(&runner, temp.path()).unwrap()
        );
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
    fn failed_refresh_does_not_mint_a_poll_token() {
        let temp = tempfile::tempdir().unwrap();
        let runner = FailingRunner {
            attempts: Cell::new(0),
        };
        let plan = refresh::Plan {
            for_diff: false,
            budget: None,
            session_dir: None,
        };
        let mut heartbeats = 0;

        let token = refresh_or_warn(&runner, temp.path(), &plan, &mut || heartbeats += 1);

        assert_eq!(token, None);
        assert_eq!(heartbeats, 0);
    }

    #[test]
    fn a_failed_refresh_says_so_in_the_packet_once_and_keeps_the_last_good_content() {
        let temp = tempfile::tempdir().unwrap();
        let md = temp.path().join(refresh::CURRENT_MD);
        std::fs::create_dir_all(md.parent().unwrap()).unwrap();
        std::fs::write(&md, "# Toimija Context\n- HEAD: abc123\n").unwrap();
        let runner = FailingRunner {
            attempts: Cell::new(0),
        };
        let plan = refresh::Plan {
            for_diff: false,
            budget: None,
            session_dir: None,
        };

        refresh_or_warn(&runner, temp.path(), &plan, &mut || {});
        let marked = std::fs::read_to_string(&md).unwrap();
        assert!(
            marked.contains("Refresh failed — this packet is stale."),
            "{marked}"
        );
        assert!(
            marked.ends_with("# Toimija Context\n- HEAD: abc123\n"),
            "the last good packet is kept under the notice: {marked}"
        );

        // The same failure on the next poll is not written again.
        let error = anyhow::anyhow!("failed to build context packet: bad config");
        assert!(plan.mark_stale(temp.path(), &error).unwrap());
        assert!(!plan.mark_stale(temp.path(), &error).unwrap());
        // A different failure replaces the notice instead of stacking.
        let other = anyhow::anyhow!("another failure");
        assert!(plan.mark_stale(temp.path(), &other).unwrap());
        let replaced = std::fs::read_to_string(&md).unwrap();
        assert_eq!(replaced.matches("Refresh failed").count(), 1, "{replaced}");
        assert!(replaced.contains("another failure"), "{replaced}");
        assert!(replaced.ends_with("- HEAD: abc123\n"), "{replaced}");
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
        assert!(!is_ignored(&PathBuf::from("/repo/.git/logs/HEAD")));
        assert!(!is_ignored(&PathBuf::from("/repo/.git/packed-refs")));
    }

    #[test]
    fn toimija_git_footprint_is_ignored_without_hiding_git_state() {
        assert!(is_ignored(&PathBuf::from("/repo/.git/objects/ab/cdef")));
        assert!(is_ignored(&PathBuf::from(
            "/repo/.git/toimija-snapshot-123.index"
        )));
        assert!(is_ignored(&PathBuf::from(
            "/repo/.git/refs/toimija/snapshots/demo"
        )));
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
    fn linux_uses_git_state_polling_instead_of_notify_polling() {
        let temp = tempfile::tempdir().unwrap();

        let session = start(temp.path()).unwrap();

        assert_eq!(session.backend_label(), "git polling");
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
        let repo_root = Path::new("/repo");
        let build_churn = notify::Error::generic("walkdir error scanning")
            .add_path(PathBuf::from("/repo/_build/lib/vartio_core/priv"));
        assert!(error_is_ignorable(&build_churn, repo_root));

        // A pathless error says nothing about where it happened.
        let pathless = notify::Error::generic("backend failure");
        assert!(!error_is_ignorable(&pathless, repo_root));

        // One relevant path makes the whole error relevant.
        let mixed = notify::Error::generic("walkdir error scanning")
            .add_path(PathBuf::from("/repo/target/debug/x"))
            .add_path(PathBuf::from("/repo/src/main.rs"));
        assert!(!error_is_ignorable(&mixed, repo_root));
    }

    #[test]
    fn poll_scan_not_found_races_stay_quiet_but_real_io_errors_surface() {
        let temp = tempfile::tempdir().unwrap();
        let repo_root = temp.path();
        let vanished_during_scan =
            notify::Error::io(std::io::Error::from(std::io::ErrorKind::NotFound))
                .add_path(repo_root.join(".git/objects/tmp_obj_abcd"));
        assert!(error_is_ignorable(&vanished_during_scan, repo_root));

        let pathless_not_found =
            notify::Error::io(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert!(!error_is_ignorable(&pathless_not_found, repo_root));

        let permission_denied =
            notify::Error::io(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
                .add_path(repo_root.join("private"));
        assert!(!error_is_ignorable(&permission_denied, repo_root));

        // Backends may canonicalize or otherwise alias the watched path. Root
        // presence, not lexical path equality, decides whether freshness is lost.
        let missing_repo_root = repo_root.join("missing");
        let missing_root_error =
            notify::Error::io(std::io::Error::from(std::io::ErrorKind::NotFound))
                .add_path(repo_root.join("different-spelling"));
        assert!(!error_is_ignorable(&missing_root_error, &missing_repo_root));
    }
}
