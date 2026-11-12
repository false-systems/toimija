//! How many git processes each command starts, pinned exactly.
//!
//! A command discovers its worktree once (one `git rev-parse` names the root,
//! the git dir, the common dir and whether the repository is shallow), reads
//! `git status --porcelain=v2 --branch` once (HEAD, the branch, the upstream
//! and ahead/behind are in its headers), and reads nothing its output does not
//! show. A change to any count below is a change to the budget, made here on
//! purpose. Counted the way an operator counts them: Git's own trace2 event
//! stream (`GIT_TRACE2_EVENT`), one `"event":"start"` per process.
//!
//! The scenario is a repository as the fleet has them: cloned from a forge
//! (`origin/HEAD` set), on a feature branch with an upstream and one commit of
//! its own, with staged, unstaged and untracked changes. Git runs with no
//! configuration from the machine, so a global `core.fsmonitor`, say, cannot
//! add processes of its own.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use tempfile::TempDir;

mod common;
use common::git_available;
use common::scenarios::{self, HERMETIC_GIT_ENV};

struct Sandbox {
    _temp: TempDir,
    home: PathBuf,
    cwd: PathBuf,
    traces: std::cell::Cell<usize>,
}

impl Sandbox {
    fn new(scenario: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let cwd = scenarios::build(scenario, &work);
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        Self {
            _temp: temp,
            home,
            cwd,
            traces: std::cell::Cell::new(0),
        }
    }

    /// Every git process `toimija <args>` starts, each argv without the
    /// program, and what it printed on stderr. The exit code must be `code`.
    fn git_processes(&self, args: &[&str], code: i32) -> (Vec<Vec<String>>, String) {
        let trace = self.home.join(format!("trace-{}.json", self.traces.get()));
        self.traces.set(self.traces.get() + 1);
        let output = Command::new(env!("CARGO_BIN_EXE_toimija"))
            .arg("-q")
            .args(args)
            .current_dir(&self.cwd)
            .envs(HERMETIC_GIT_ENV)
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", self.home.join("state"))
            .env("TOIMIJA_STATE_DIR", self.home.join("toimija"))
            .env("GIT_TRACE2_EVENT", &trace)
            .env_remove("TOIMIJA_SESSION")
            .env_remove("TOIMIJA_RUN")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(
            output.status.code(),
            Some(code),
            "toimija {args:?}: {stderr}"
        );
        let calls = std::fs::read_to_string(&trace)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| event["event"] == "start")
            .map(|event| {
                event["argv"].as_array().unwrap()[1..]
                    .iter()
                    .map(|arg| arg.as_str().unwrap().to_string())
                    .collect()
            })
            .collect();
        (calls, stderr)
    }

    fn assert_budget(&self, args: &[&str], code: i32, budget: usize) -> Vec<Vec<String>> {
        let (calls, _) = self.git_processes(args, code);
        assert_eq!(calls.len(), budget, "toimija {args:?}: {calls:#?}");
        calls
    }
}

/// The git subcommand of an argv, past global options such as
/// `--no-optional-locks`.
fn verb(argv: &[String]) -> &str {
    argv.iter()
        .map(String::as_str)
        .find(|arg| !arg.starts_with('-'))
        .unwrap_or("")
}

fn verbs(calls: &[Vec<String>]) -> Vec<&str> {
    calls.iter().map(|argv| verb(argv)).collect()
}

/// The packet: the discovery, `status`, the base (`origin/HEAD`), the
/// merge-base, the branch diff and the recent commits.
const PACKET: [&str; 6] = [
    "rev-parse",
    "status",
    "symbolic-ref",
    "merge-base",
    "diff",
    "log",
];

#[test]
fn the_packet_commands_read_the_worktree_once() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("dirty");
    for args in [
        &["enter", "--json"][..],
        &["enter"],
        &["enter", "--for-diff"],
    ] {
        let calls = sandbox.assert_budget(args, 0, 6);
        assert_eq!(verbs(&calls), PACKET, "toimija {args:?}: {calls:#?}");
    }
}

#[test]
fn the_reports_find_the_root_once() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("dirty");
    for args in [&["sessions", "--json"][..], &["sessions"]] {
        let calls = sandbox.assert_budget(args, 0, 1);
        assert_eq!(
            calls[0],
            ["rev-parse", "--show-toplevel"],
            "toimija {args:?}"
        );
    }
}

/// `run` costs `enter`'s six and one more, however long the agent runs: the
/// packet at launch, and one `git status` when the agent exits, for the
/// summary. While the agent runs, toimija starts no git process: an agent that
/// sleeps costs what one that exits at once does.
#[test]
fn run_costs_enter_and_one_however_long_the_agent_runs() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("dirty");
    for agent in [&["true"][..], &["sh", "-c", "sleep 3"]] {
        let mut args = vec!["run", "--intent", "budget", "--"];
        args.extend_from_slice(agent);
        let calls = sandbox.assert_budget(&args, 0, 7);
        let verbs = verbs(&calls);
        assert_eq!(verbs[..6], PACKET, "{agent:?}: {calls:#?}");
        assert_eq!(
            calls[6][..2],
            ["--no-optional-locks", "status"],
            "{calls:#?}"
        );
    }
}

/// The counts that depend on where the worktree is and what it tracks.
#[test]
fn what_the_packet_costs_depends_on_what_there_is_to_read() {
    if !git_available() {
        return;
    }
    // On the base itself: the branch adds no commits, so there is no diff.
    let clean = Sandbox::new("clean");
    let calls = clean.assert_budget(&["enter", "--json"], 0, 5);
    assert!(!verbs(&calls).contains(&"diff"), "{calls:#?}");
    // No upstream: ahead/behind are counted against `origin/HEAD`'s branch.
    let no_upstream = Sandbox::new("no-upstream");
    let calls = no_upstream.assert_budget(&["enter", "--json"], 0, 7);
    assert_eq!(verb(calls.last().unwrap()), "rev-list", "{calls:#?}");
    // From a subdirectory, the discovery is asked again at the root, where
    // the relative answers are the root's.
    let below = Sandbox::new("subdirectory");
    let calls = below.assert_budget(&["enter", "--json"], 0, 7);
    assert_eq!(verbs(&calls)[..2], ["rev-parse", "rev-parse"], "{calls:#?}");
    // A linked worktree costs what a plain checkout does: this one is on its
    // base too, so it is the clean checkout's five.
    let linked = Sandbox::new("linked-worktree");
    let calls = linked.assert_budget(&["enter", "--json"], 0, 5);
    assert_eq!(
        verbs(&calls),
        verbs(&clean.git_processes(&["enter", "--json"], 0).0)
    );
}

#[test]
fn a_sandbox_traces_its_own_git() {
    // Positive control for every count above: the trace sees a process the
    // test itself starts, so a count of zero is never a blind trace.
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("clean");
    let trace = sandbox.home.join("control.json");
    let status = common::git_at(&sandbox.cwd)
        .envs(HERMETIC_GIT_ENV)
        .env("GIT_TRACE2_EVENT", &trace)
        .args(["rev-parse", "HEAD"])
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let starts = std::fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .filter(|line| line.contains(r#""event":"start""#))
        .count();
    assert_eq!(starts, 1);
}

/// `handover` finds the root, and reads HEAD and its branch in one process.
#[test]
fn handover_costs_two() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("dirty");
    let calls = sandbox.assert_budget(&["handover", "--next", "the lexer is done"], 0, 2);
    assert_eq!(calls[0], ["rev-parse", "--show-toplevel"], "{calls:#?}");
    assert_eq!(
        calls[1],
        ["rev-parse", "HEAD", "--abbrev-ref", "HEAD"],
        "{calls:#?}"
    );
}
