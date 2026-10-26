use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

// `watch` is a foreground process that refreshes `.toimija/current.*` on change.
// This drives it end-to-end: start it, confirm the baseline refresh, change
// the repo, confirm the refresh, then stop.

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
    let repo_root = temp.path().canonicalize().unwrap();
    init_git_repo(&repo_root);
    let current = repo_root.join(".toimija/current.json");

    let mut child = Command::new(env!("CARGO_BIN_EXE_toimija"))
        .arg("watch")
        .current_dir(&repo_root)
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
}

/// A repository with one commit on `main`.
fn init_main(path: &Path) {
    run_git(path, &["init", "-q", "-b", "main"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    run_git(path, &["config", "commit.gpgsign", "false"]);
    std::fs::write(path.join("a.txt"), "x\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-q", "-m", "init"]);
}

/// A `toimija watch` child, sandboxed (its own HOME and state dir), armed
/// before `start` returns and killed when dropped — a failing assertion never
/// leaks the process.
struct Watching {
    child: std::process::Child,
    lines: mpsc::Receiver<String>,
    packet: std::path::PathBuf,
    _home: tempfile::TempDir,
}

impl Watching {
    fn start(root: &Path) -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_toimija"))
            .arg("watch")
            .current_dir(root)
            .env("HOME", home.path())
            .env("XDG_STATE_HOME", home.path().join("state"))
            .env("TOIMIJA_STATE_DIR", home.path().join("toimija"))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("watch should start");
        let stderr = child.stderr.take().expect("watch stderr should be piped");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stderr = BufReader::new(stderr);
            let mut line = String::new();
            while stderr.read_line(&mut line).unwrap_or(0) != 0 {
                if tx.send(line.clone()).is_err() {
                    break;
                }
                line.clear();
            }
        });
        let mut watching = Self {
            child,
            lines,
            packet: root.join(".toimija/current.json"),
            _home: home,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = String::new();
        while let Ok(line) = watching
            .lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            seen.push_str(&line);
            if line.starts_with("toimija watch: armed (") {
                return watching;
            }
        }
        let _ = watching.child.kill();
        panic!("watch should report readiness; stderr: {seen}");
    }

    fn packet(&self) -> serde_json::Value {
        std::fs::read_to_string(&self.packet)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(serde_json::Value::Null)
    }

    /// Wait for the live packet to show `what`. Ten seconds is CI slack: a
    /// change lands in well under a second.
    fn wait_for(&self, what: &str, shown: impl Fn(&serde_json::Value) -> bool) {
        if wait_until(Duration::from_secs(10), || shown(&self.packet())) {
            return;
        }
        let stderr: String = self.lines.try_iter().collect();
        panic!(
            "watch did not refresh for {what}; stderr: {stderr}; packet git: {}",
            self.packet()["git"]
        );
    }
}

impl Drop for Watching {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn lists(packet: &serde_json::Value, field: &str, path: &str) -> bool {
    packet["git"][field].as_array().is_some_and(|files| {
        files
            .iter()
            .any(|file| file == path || file["path"] == path)
    })
}

/// A bare `origin.git`, a clone `primary` of it, and a linked worktree
/// `linked` on branch `feature` tracking `origin/main`. The linked worktree's
/// git dir is under `primary/.git/worktrees/`, outside the watched root, so no
/// event backend reports a change made there: the watcher's stamp of the Git
/// files has to.
struct Linked {
    _temp: tempfile::TempDir,
    base: std::path::PathBuf,
    primary: std::path::PathBuf,
    linked: std::path::PathBuf,
}

fn linked_worktree() -> Linked {
    let temp = watch_tempdir();
    let base = temp.path().canonicalize().unwrap();
    let seed = base.join("seed");
    std::fs::create_dir(&seed).unwrap();
    init_main(&seed);
    run_git(&base, &["clone", "-q", "--bare", "seed", "origin.git"]);
    run_git(&base, &["clone", "-q", "origin.git", "primary"]);
    let primary = base.join("primary");
    run_git(
        &primary,
        &["config", "user.email", "toimija@example.invalid"],
    );
    run_git(&primary, &["config", "user.name", "Toimija Test"]);
    run_git(&primary, &["config", "commit.gpgsign", "false"]);
    let linked = base.join("linked");
    run_git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            linked.to_str().unwrap(),
            "origin/main",
        ],
    );
    run_git(
        &linked,
        &["branch", "-q", "--set-upstream-to", "origin/main"],
    );
    Linked {
        _temp: temp,
        base,
        primary,
        linked,
    }
}

#[test]
fn watch_sees_a_work_tree_edit_in_a_repository_under_an_ignored_name() {
    // Ignored names are matched relative to the repository root: a checkout
    // under `target/` or `.sykli/` still sees its own edits.
    if !git_available() {
        return;
    }
    let temp = watch_tempdir();
    let root = temp
        .path()
        .canonicalize()
        .unwrap()
        .join("target/.sykli/repo");
    std::fs::create_dir_all(&root).unwrap();
    init_main(&root);
    let watch = Watching::start(&root);

    std::fs::write(root.join("new.rs"), "// new\n").unwrap();

    watch.wait_for("a new file", |packet| {
        lists(packet, "untracked_files", "new.rs")
    });
}

#[test]
fn watch_sees_staging_in_a_linked_worktree() {
    if !git_available() {
        return;
    }
    let fixture = linked_worktree();
    let watch = Watching::start(&fixture.linked);
    std::fs::write(fixture.linked.join("a.txt"), "edited\n").unwrap();
    watch.wait_for("an edit", |packet| lists(packet, "unstaged_files", "a.txt"));

    run_git(&fixture.linked, &["add", "a.txt"]);

    watch.wait_for("staging", |packet| {
        lists(packet, "staged_files", "a.txt") && !lists(packet, "unstaged_files", "a.txt")
    });
}

#[test]
fn watch_sees_a_commit_in_a_linked_worktree() {
    if !git_available() {
        return;
    }
    let fixture = linked_worktree();
    let watch = Watching::start(&fixture.linked);
    let before = watch.packet()["git"]["head_oid"].clone();

    run_git(
        &fixture.linked,
        &["commit", "-q", "--allow-empty", "-m", "next"],
    );

    watch.wait_for("a commit", |packet| {
        packet["git"]["head_oid"] != before && packet["git"]["ahead"] == 1
    });
}

#[test]
fn watch_sees_a_branch_switch_in_a_linked_worktree() {
    if !git_available() {
        return;
    }
    let fixture = linked_worktree();
    let watch = Watching::start(&fixture.linked);

    run_git(&fixture.linked, &["switch", "-q", "-c", "other"]);

    watch.wait_for("a branch switch", |packet| {
        packet["git"]["branch"] == "other"
    });
}

#[test]
fn watch_sees_a_fetch_move_the_upstream() {
    // The fetch runs in another worktree of the same repository: the shared
    // `refs/remotes/origin/main` moves, and the watched worktree falls behind.
    if !git_available() {
        return;
    }
    let fixture = linked_worktree();
    let watch = Watching::start(&fixture.linked);
    let pusher = fixture.base.join("pusher");
    run_git(&fixture.base, &["clone", "-q", "origin.git", "pusher"]);
    run_git(
        &pusher,
        &["config", "user.email", "toimija@example.invalid"],
    );
    run_git(&pusher, &["config", "user.name", "Toimija Test"]);
    run_git(&pusher, &["config", "commit.gpgsign", "false"]);
    run_git(
        &pusher,
        &["commit", "-q", "--allow-empty", "-m", "upstream moved"],
    );
    run_git(&pusher, &["push", "-q", "origin", "main"]);
    assert_eq!(watch.packet()["git"]["behind"], 0);

    run_git(&fixture.primary, &["fetch", "-q", "origin"]);

    watch.wait_for("a fetch", |packet| packet["git"]["behind"] == 1);
}

#[test]
fn watch_sees_a_worktree_added_elsewhere() {
    // With no remote, the base is a local `main` or `master`. A worktree added
    // elsewhere with a new `main` branch gives the watched worktree its base.
    if !git_available() {
        return;
    }
    let temp = watch_tempdir();
    let base = temp.path().canonicalize().unwrap();
    let primary = base.join("primary");
    std::fs::create_dir(&primary).unwrap();
    init_main(&primary);
    run_git(&primary, &["branch", "-q", "-m", "trunk"]);
    let watched = base.join("watched");
    run_git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            watched.to_str().unwrap(),
        ],
    );
    let watch = Watching::start(&watched);
    assert_eq!(watch.packet()["git"]["base"], serde_json::Value::Null);

    let elsewhere = base.join("elsewhere");
    run_git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "main",
            elsewhere.to_str().unwrap(),
        ],
    );

    watch.wait_for("a worktree added elsewhere", |packet| {
        packet["git"]["base"] == "main"
    });
}

#[test]
fn watch_sees_a_commit_in_a_plain_checkout() {
    // Here `.git/` is inside the watched root: its events only prompt the
    // stamp check, which sees the commit.
    if !git_available() {
        return;
    }
    let temp = watch_tempdir();
    let root = temp.path().canonicalize().unwrap();
    init_main(&root);
    let watch = Watching::start(&root);
    let before = watch.packet()["git"]["head_oid"].clone();

    run_git(&root, &["commit", "-q", "--allow-empty", "-m", "next"]);

    watch.wait_for("a commit", |packet| packet["git"]["head_oid"] != before);
}

/// Git processes `toimija <args>` starts in `root`, counted from Git's own
/// trace2 event stream.
fn git_processes(root: &Path, home: &Path, args: &[&str]) -> usize {
    let trace = home.join(format!("trace-{}.json", args.join("-").replace('/', "_")));
    let _ = std::fs::remove_file(&trace);
    let output = Command::new(env!("CARGO_BIN_EXE_toimija"))
        .args(args)
        .current_dir(root)
        .env("HOME", home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env("TOIMIJA_STATE_DIR", home.join("toimija"))
        .env("GIT_TRACE2_EVENT", &trace)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "toimija {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read_to_string(&trace)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(r#""event":"start""#))
        .count()
}

#[test]
fn run_costs_enter_plus_a_constant_however_long_the_agent_runs() {
    if !git_available() {
        return;
    }
    let temp = watch_tempdir();
    let root = temp.path().canonicalize().unwrap().join("repo");
    std::fs::create_dir(&root).unwrap();
    init_main(&root);
    let home = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();

    let enter = git_processes(&root, &home, &["enter"]);
    let quick = git_processes(&root, &home, &["run", "--intent", "budget", "--", "true"]);
    let slow = git_processes(
        &root,
        &home,
        &["run", "--intent", "budget", "--", "sleep", "2"],
    );

    // Linux's Git polling spends one `git status` per 500 ms tick; the event
    // backends spend none while nothing changes.
    let per_tick = usize::from(cfg!(target_os = "linux"));
    assert!(
        slow <= quick + 4 * per_tick + 1,
        "two seconds of an idle agent cost {} git processes (run -- true: {quick}, run -- sleep 2: {slow})",
        slow.saturating_sub(quick)
    );
    // What `run` does beyond `enter`, once, whatever the duration: find the
    // git dir (1), write the repository point (6), the entry gate's re-read
    // (12), the catch-up probe (1) and the exit refresh (12).
    assert!(
        slow <= enter + 34 + 5 * per_tick,
        "run -- sleep 2 started {slow} git processes, enter {enter}"
    );
}
