//! `toimija watch` — keep `.toimija/current.*` fresh while you work.
//!
//! A foreground command (NOT a daemon): it watches the repo for filesystem and
//! Git-state changes and re-runs [`refresh`](crate::refresh) after a debounce
//! window, until it is stopped (Ctrl-C on the process, or the `stop` flag that
//! `run` sets). No network, no validation execution. Events under build/vendor
//! directories and the live files themselves are ignored, so writing
//! `.toimija/current.*` never re-triggers a refresh.

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
    /// Refresh the live packet on every relevant change until `stop` is set.
    /// `on_refresh` is called after each refresh (for status logging).
    pub fn run(
        self,
        runner: &impl CommandRunner,
        repo_root: &Path,
        for_diff: bool,
        budget: Option<usize>,
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
                    refresh::refresh(runner, repo_root, for_diff, budget)?;
                    on_refresh();
                }
                // Irrelevant event, or a watcher-side error: keep watching.
                Ok(_) => {}
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
    for_diff: bool,
    budget: Option<usize>,
    stop: &AtomicBool,
    on_refresh: impl FnMut(),
) -> Result<()> {
    // Baseline so the live packet exists immediately, before any event.
    refresh::refresh(runner, repo_root, for_diff, budget)?;
    let session = start(repo_root)?;
    session.run(runner, repo_root, for_diff, budget, stop, on_refresh)
}

/// Swallow the rest of an event burst: keep receiving until the channel is quiet
/// for a full debounce window, so one logical change yields one refresh.
fn drain_burst(rx: &mpsc::Receiver<notify::Result<Event>>) {
    while rx.recv_timeout(DEBOUNCE).is_ok() {}
}

fn relevant(event: &Event) -> bool {
    event.paths.iter().any(|path| !is_ignored(path))
}

/// Ignore the live files (so our own writes never re-trigger) and anything under
/// a build/vendor directory. Matched on path tails/components so it is robust to
/// the absolute, possibly symlink-resolved paths notify reports.
fn is_ignored(path: &Path) -> bool {
    if is_live_file(path) {
        return true;
    }
    path.components().any(|component| {
        matches!(component, Component::Normal(name) if IGNORED_DIRS.iter().any(|dir| name == *dir))
    })
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
