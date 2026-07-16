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
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{Config, Event, RecursiveMode, Watcher};

use crate::command::CommandRunner;
use crate::{consistency, refresh};

/// Coalesce a burst of events for this long before refreshing once.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the wait loop wakes to re-check the stop flag.
const TICK: Duration = Duration::from_millis(200);
/// Directories whose churn must never trigger a refresh.
const IGNORED_DIRS: [&str; 4] = ["target", "_build", "deps", "node_modules"];

type BackendWatcher = notify::PollWatcher;

/// An active filesystem watch. The fallible OS setup is done in [`start`], so a
/// caller can report a startup failure *before* doing other work; the blocking
/// refresh loop is [`Session::run`]. Dropping the session stops watching.
pub struct Session {
    // Held only to keep the OS watch alive; dropping it unregisters the watch.
    _watcher: BackendWatcher,
    rx: mpsc::Receiver<notify::Result<Event>>,
}

/// Begin watching `repo_root`. The fallible parts — creating the watcher and
/// registering the path — happen here, synchronously, so a startup failure is
/// reported to the caller rather than swallowed by a background loop.
pub fn start(repo_root: &Path) -> Result<Session> {
    let (tx, rx) = mpsc::channel();
    // ponytail: native backends can miss the first event in CI/temp volumes;
    // bounded polling keeps freshness observable at the cost of scan work.
    let mut watcher = BackendWatcher::new(
        move |res| {
            let _ = tx.send(res);
        },
        watcher_config(),
    )
    .context("failed to create filesystem watcher")?;
    watcher
        .watch(repo_root, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", repo_root.display()))?;
    let probe = repo_root.join(format!("toimija-watch-probe-{}", std::process::id()));
    fs::write(&probe, b"watch readiness probe")
        .with_context(|| format!("failed to arm watcher at {}", repo_root.display()))?;
    watcher
        .poll()
        .context("failed to request filesystem watcher readiness poll")?;
    let ready = loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(event)) if event.paths.iter().any(|path| path == &probe) => break true,
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => break false,
            Err(mpsc::RecvTimeoutError::Disconnected) => break false,
        }
    };
    let _ = fs::remove_file(&probe);
    anyhow::ensure!(ready, "filesystem watcher did not report readiness");
    Ok(Session {
        _watcher: watcher,
        rx,
    })
}

fn watcher_config() -> Config {
    Config::default().with_poll_interval(Duration::from_millis(500))
}

impl Session {
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
        mut on_refresh: impl FnMut(),
    ) -> Result<()> {
        while !stop.load(Ordering::Relaxed) {
            match self.rx.recv_timeout(TICK) {
                Ok(Ok(event)) if relevant(&event) => {
                    drain_burst(&self.rx);
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    // A transient refresh failure must not kill live updates for
                    // the rest of the session: warn and keep watching, so the
                    // packet recovers on the next change rather than freezing.
                    match plan.run(runner, repo_root) {
                        Ok(_) => on_refresh(),
                        Err(error) => {
                            eprintln!("toimija: refresh failed (packet may be stale): {error:#}");
                        }
                    }
                }
                // A watcher-backend error (dropped or rescan-required events):
                // surface it, since the packet may now be stale, then keep going.
                Ok(Err(error)) => {
                    eprintln!("toimija: file watcher error (packet may be stale): {error}");
                }
                // Irrelevant event: keep watching.
                Ok(Ok(_)) => {}
                // Woke to re-check `stop`.
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                // Watcher dropped its sender — nothing more will arrive.
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(())
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
    eprintln!("toimija watch: armed");
    session.run(runner, repo_root, plan, stop, on_refresh)
}

/// Swallow the rest of an event burst: keep receiving until the channel is quiet
/// for a full debounce window, so one logical change yields one refresh.
fn drain_burst(rx: &mpsc::Receiver<notify::Result<Event>>) {
    while rx.recv_timeout(DEBOUNCE).is_ok() {}
}

fn relevant(event: &Event) -> bool {
    event.paths.iter().any(|path| !is_ignored(path))
}

/// Ignore generated Toimija output (so our own writes never re-trigger) and
/// anything under a build/vendor directory. Matched on path tails/components so
/// it is robust to the absolute, possibly symlink-resolved paths notify reports.
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
        .position(|part| *part == ".toimija")
        .is_some_and(|index| consistency::is_generated_output(&PathBuf::from_iter(&parts[index..])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn live_files_are_ignored() {
        assert!(is_ignored(&PathBuf::from("/repo/.toimija/current.md")));
        assert!(is_ignored(&PathBuf::from("/repo/.toimija/current.json")));
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
}
