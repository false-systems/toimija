use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// `watch` is a foreground process that refreshes `.toimija/current.*` on change.
// This drives it end-to-end: start it, confirm the baseline refresh, change a
// file, confirm a debounced refresh picked it up, then stop the process.

mod common;
use common::{git_available, run_git};

fn init_git_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    std::fs::write(path.join("a.txt"), "x\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "init"]);
}

/// Poll `cond` until true or `timeout` elapses.
fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

#[test]
fn watch_refreshes_on_a_file_change_then_stops() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    let current = temp.path().join(".toimija/current.json");

    let mut child = Command::new(env!("CARGO_BIN_EXE_toimija"))
        .arg("watch")
        .current_dir(temp.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("watch should start");

    // Baseline refresh happens at startup.
    let started = wait_until(Duration::from_secs(10), || current.exists());
    assert!(started, "watch should write the live packet on startup");
    let before = std::fs::read_to_string(&current).unwrap();
    assert!(!before.contains("late_arrival.rs"));

    // Re-trigger the change while polling. Filesystem events can be dropped or
    // delayed under load (notably inotify in CI), so a single write is flaky;
    // repeatedly touching the file (a CREATE then MODIFY events) until the live
    // packet reflects it is robust without weakening the assertion.
    let target = temp.path().join("late_arrival.rs");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut refreshed = false;
    let mut nudge = 0u32;
    while Instant::now() < deadline {
        nudge += 1;
        std::fs::write(&target, format!("// change {nudge}\n")).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        if std::fs::read_to_string(&current)
            .map(|contents| contents.contains("late_arrival.rs"))
            .unwrap_or(false)
        {
            refreshed = true;
            break;
        }
    }

    // Stop the watcher before asserting, so a failure never leaks the process.
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        refreshed,
        "watch should refresh the live packet after a file change"
    );
}
