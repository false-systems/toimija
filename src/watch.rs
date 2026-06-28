//! `toimija watch` — keep the live packet fresh while you work.
//!
//! A foreground command (NOT a daemon): it watches the repo for filesystem and
//! Git-state changes and re-runs a [`refresh::Plan`](crate::refresh::Plan) after
//! a debounce window, until it is stopped (Ctrl-C on the process, or the `stop`
//! flag that `run` drives). No network, no validation execution. Events under
//! build/vendor directories and Toimija's own live output are ignored, so writing
//! the live packet — `.toimija/current.*`, or a per-session
//! `.toimija/sessions/<id>/current.*` under `run` — never re-triggers a refresh.

use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{Event, RecursiveMode, Watcher};

use crate::command::CommandRunner;
use crate::refresh;

/// Coalesce a burst of events for this long before refreshing once.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the wait loop wakes to re-check the stop flag.
const TICK: Duration = Duration::from_millis(200);
/// Directories whose churn must never trigger a refresh.
const IGNORED_DIRS: [&str; 4] = ["target", "_build", "deps", "node_modules"];

/// An active filesystem watch. The fallible OS setup is done in [`start`], so a
/// caller can report a startup failure *before* doing other work; the blocking
/// refresh loop is [`Session::run`]. Dropping the session stops watching.
pub struct Session {
    // Held only to keep the OS watch alive; dropping it unregisters the watch.
    _watcher: notify::RecommendedWatcher,
    rx: mpsc::Receiver<notify::Result<Event>>,
}

/// Begin watching `repo_root`. The fallible parts — creating the watcher and
/// registering the path — happen here, synchronously, so a startup failure is
/// reported to the caller rather than swallowed by a background loop.
pub fn start(repo_root: &Path) -> Result<Session> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .context("failed to create filesystem watcher")?;
    watcher
        .watch(repo_root, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", repo_root.display()))?;
    Ok(Session {
        _watcher: watcher,
        rx,
    })
}

impl Session {
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
/// and run the loop. Startup failures propagate via `?`.
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

/// Ignore the live files and per-session output (so our own writes never
/// re-trigger) and anything under a build/vendor directory. Matched on path
/// tails/components so it is robust to the absolute, possibly symlink-resolved
/// paths notify reports.
fn is_ignored(path: &Path) -> bool {
    if is_live_file(path) || is_in_sessions(path) {
        return true;
    }
    path.components().any(|component| {
        matches!(component, Component::Normal(name) if IGNORED_DIRS.iter().any(|dir| name == *dir))
    })
}

/// Anything under `.toimija/sessions/` is Toimija's own per-session output;
/// refreshing a session packet must not re-trigger the watch. Matched on the
/// contiguous `.toimija/sessions` component pair so it survives absolute paths.
fn is_in_sessions(path: &Path) -> bool {
    let names: Vec<&str> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    names
        .windows(2)
        .any(|pair| pair == [".toimija", "sessions"])
}

fn is_live_file(path: &Path) -> bool {
    let in_toimija = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some(".toimija");
    let name = path.file_name().and_then(|name| name.to_str());
    in_toimija && matches!(name, Some("current.md") | Some("current.json"))
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
