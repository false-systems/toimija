use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

// `watch` is a foreground process that refreshes `.toimija/current.*` on change.
// This drives it end-to-end: start it, confirm the baseline refresh, change
// repo and external provider state, confirm both refreshes, then stop.

mod common;
use common::{git_available, run_git};

fn watch_tempdir() -> tempfile::TempDir {
    #[cfg(target_os = "macos")]
    {
        tempfile::Builder::new()
            .prefix("toimija-watch-")
            .tempdir_in("/private/tmp")
            .unwrap()
    }
    #[cfg(not(target_os = "macos"))]
    {
        tempfile::Builder::new()
            .prefix("toimija-watch-")
            .tempdir()
            .unwrap()
    }
}

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
    let temp = watch_tempdir();
    let config = tempfile::tempdir().unwrap();
    let repo_root = temp.path().canonicalize().unwrap();
    init_git_repo(&repo_root);
    std::fs::create_dir_all(repo_root.join(".toimija")).unwrap();
    std::fs::write(
        repo_root.join(".toimija/repo.yaml"),
        "composition:\n  work: custom-work\n",
    )
    .unwrap();
    run_git(&repo_root, &["add", ".toimija/repo.yaml"]);
    run_git(&repo_root, &["commit", "-m", "configure work provider"]);
    let current = repo_root.join(".toimija/current.json");

    let mut child = Command::new(env!("CARGO_BIN_EXE_toimija"))
        .arg("watch")
        .current_dir(&repo_root)
        .env("XDG_CONFIG_HOME", config.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("watch should start");

    let stderr = child.stderr.take().expect("watch stderr should be piped");
    let (stderr_tx, stderr_rx) = mpsc::channel();
    let stderr_reader = std::thread::spawn(move || {
        let mut stderr = BufReader::new(stderr);
        let mut line = String::new();
        while stderr.read_line(&mut line).unwrap_or(0) != 0 {
            if stderr_tx.send(line.clone()).is_err() {
                break;
            }
            line.clear();
        }
    });
    let mut armed = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut diagnostics = String::new();
    while let Ok(line) = stderr_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        diagnostics.push_str(&line);
        if matches!(
            line.trim(),
            "toimija watch: armed (native)"
                | "toimija watch: armed (poll fallback)"
                | "toimija watch: armed (git polling)"
        ) {
            armed = true;
            break;
        }
    }
    if !armed {
        let _ = child.kill();
        let _ = child.wait();
        let _ = stderr_reader.join();
        diagnostics.extend(stderr_rx.try_iter());
        panic!("watch should report readiness after registration; stderr: {diagnostics}");
    }
    assert!(
        current.exists(),
        "watch should write the live packet on startup"
    );
    let before = std::fs::read_to_string(&current).unwrap();
    assert!(!before.contains("late_arrival.rs"));

    // Write once after the readiness handshake. The watcher owns the catch-up
    // refresh for the registration gap; this test must not hide missed events
    // with retries.
    let target = repo_root.join("late_arrival.rs");
    std::fs::write(&target, "// change\n").unwrap();
    let refreshed = wait_until(Duration::from_secs(5), || {
        std::fs::read_to_string(&current)
            .map(|contents| contents.contains("late_arrival.rs"))
            .unwrap_or(false)
    });
    let plugin_dir = config.path().join("toimija");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugins.toml"),
        "[plugins.custom-work]\nkind = \"work\"\ncommand = \"custom-work\"\n",
    )
    .unwrap();
    let provider_refreshed = wait_until(Duration::from_secs(5), || {
        std::fs::read_to_string(&current)
            .ok()
            .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
            .is_some_and(|packet| packet["runtime_providers"]["work"]["status"] == "configured")
    });

    // Stop the watcher before asserting, so a failure never leaks the process.
    let _ = child.kill();
    let _ = child.wait();
    let _ = stderr_reader.join();
    diagnostics.extend(stderr_rx.try_iter());

    assert!(
        refreshed,
        "watch should refresh the live packet after a file change; stderr: {diagnostics}; packet: {}",
        std::fs::read_to_string(&current).unwrap_or_default()
    );
    assert!(
        provider_refreshed,
        "watch should refresh after external provider state changes; stderr: {diagnostics}; packet: {}",
        std::fs::read_to_string(&current).unwrap_or_default()
    );
}
