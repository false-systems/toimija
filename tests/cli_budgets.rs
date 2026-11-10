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
        &["context", "--json"][..],
        &["context"],
        &["enter"],
        &["refresh"],
    ] {
        let calls = sandbox.assert_budget(args, 0, 6);
        assert_eq!(verbs(&calls), PACKET, "toimija {args:?}: {calls:#?}");
    }
}

#[test]
fn verify_reads_the_refs_only_when_the_token_holds() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("dirty");
    sandbox.git_processes(&["enter"], 0);
    // Verified: the token holds, so the refresh token is compared too.
    let calls = sandbox.assert_budget(&["verify"], 0, 6);
    assert_eq!(verbs(&calls), PACKET, "{calls:#?}");
    // Stale: the token alone says so, from one discovery and one `status`.
    std::fs::write(sandbox.cwd.join("late.txt"), "late\n").unwrap();
    let calls = sandbox.assert_budget(&["verify"], 1, 2);
    assert_eq!(verbs(&calls), ["rev-parse", "status"], "{calls:#?}");
}

#[test]
fn a_repository_point_reads_only_what_the_token_fingerprints() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("dirty");
    // The discovery, the token's `status`, the tree, the `status` after it.
    let tree = [
        "rev-parse",
        "status",
        "read-tree",
        "add",
        "rm",
        "add",
        "write-tree",
        "status",
    ];
    for args in [&["repository-point", "--json"][..], &["snapshot", "--json"]] {
        let calls = sandbox.assert_budget(args, 0, 8);
        assert_eq!(verbs(&calls), tree, "toimija {args:?}: {calls:#?}");
    }
}

/// What ohjaa asks every repository of the fleet: `repo.name`, `repo.role` and
/// `repo.not_roles`, the same object `context --json` carries, for one git
/// process where the packet costs six.
#[test]
fn the_identity_costs_one_git_process_and_is_the_packets_repo() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("dirty");
    let calls = sandbox.assert_budget(&["context", "--identity", "--json"], 0, 1);
    assert_eq!(calls[0], ["rev-parse", "--show-toplevel"]);

    let json = |args: &[&str]| -> serde_json::Value {
        let output = Command::new(env!("CARGO_BIN_EXE_toimija"))
            .args(args)
            .current_dir(&sandbox.cwd)
            .envs(HERMETIC_GIT_ENV)
            .env("HOME", &sandbox.home)
            .output()
            .unwrap();
        assert!(output.status.success());
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let identity = json(&["context", "--identity", "--json"]);
    let packet = json(&["context", "--json"]);
    assert_eq!(identity["schema"], "toimija-identity.v1");
    assert_eq!(identity["repo"], packet["repo"]);
    assert_eq!(identity["repo"]["name"], "fixture");
    assert_eq!(identity["repo"]["role"], "a repository in a known state");
    assert_eq!(
        identity["repo"]["not_roles"],
        serde_json::json!(["platform"])
    );
    // Which keys, not their order: `cargo test --workspace` unifies xtask's
    // `serde_json/preserve_order` into this build, and a parsed `Value` then
    // keeps the order it read instead of sorting.
    let mut keys: Vec<&String> = identity.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["repo", "schema"]);
}

#[test]
fn the_reports_find_the_root_once() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("dirty");
    for args in [
        &["sessions", "--json"][..],
        &["sessions"],
        &["validate", "--plan"],
    ] {
        let calls = sandbox.assert_budget(args, 0, 1);
        assert_eq!(
            calls[0],
            ["rev-parse", "--show-toplevel"],
            "toimija {args:?}"
        );
    }
}

/// `landed` costs five, then two for each path the branch touched whose
/// change is not yet in the base (its patch, and the forward `apply --check`
/// that shows it still applies). The branch here touched one.
#[test]
fn landed_reads_the_root_and_head_together() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("dirty");
    let calls = sandbox.assert_budget(&["landed", "--json"], 0, 7);
    assert_eq!(
        calls[0],
        ["rev-parse", "--show-toplevel", "--short", "HEAD"]
    );
    assert_eq!(
        verbs(&calls),
        [
            "rev-parse",
            "rev-parse",
            "merge-base",
            "diff",
            "read-tree",
            "diff",
            "apply"
        ],
        "{calls:#?}"
    );
}

/// `run` costs `enter`'s six and nine more, however long the agent runs: the
/// repository point (6), the entry gate's `status` (1), the catch-up probe
/// (1) and the exit refresh's `status` (1). The discovery names the git dir
/// and the common dir the watcher stamps; with the refs unmoved, the gate and
/// the exit carry the entry packet's ref-derived facts.
#[test]
fn run_costs_enter_and_nine() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("dirty");
    let (calls, stderr) = sandbox.git_processes(&["run", "--intent", "budget", "--", "true"], 0);
    // Git polling, the fallback when inotify cannot arm, adds one `status` a
    // tick while the agent runs.
    let polled = if stderr.contains("watcher armed (git polling)") {
        calls.len().saturating_sub(15)
    } else {
        0
    };
    assert!(polled <= 2, "{calls:#?}");
    assert_eq!(calls.len() - polled, 15, "{stderr}\n{calls:#?}");
    let verbs = verbs(&calls);
    assert_eq!(verbs[..6], PACKET, "{calls:#?}");
    assert_eq!(
        verbs[6..12],
        ["read-tree", "add", "rm", "add", "write-tree", "status"],
        "{calls:#?}"
    );
    assert!(
        verbs[12..].iter().all(|verb| *verb == "status"),
        "{calls:#?}"
    );
}

/// The counts that depend on where the worktree is and what it tracks.
#[test]
fn what_the_packet_costs_depends_on_what_there_is_to_read() {
    if !git_available() {
        return;
    }
    // On the base itself: the branch adds no commits, so there is no diff.
    let clean = Sandbox::new("clean");
    let calls = clean.assert_budget(&["context", "--json"], 0, 5);
    assert!(!verbs(&calls).contains(&"diff"), "{calls:#?}");
    // No upstream: ahead/behind are counted against `origin/HEAD`'s branch.
    let no_upstream = Sandbox::new("no-upstream");
    let calls = no_upstream.assert_budget(&["context", "--json"], 0, 7);
    assert_eq!(verb(calls.last().unwrap()), "rev-list", "{calls:#?}");
    // From a subdirectory, the discovery is asked again at the root, where
    // the relative answers are the root's.
    let below = Sandbox::new("subdirectory");
    let calls = below.assert_budget(&["context", "--json"], 0, 7);
    assert_eq!(verbs(&calls)[..2], ["rev-parse", "rev-parse"], "{calls:#?}");
    // A linked worktree costs what a plain checkout does: this one is on its
    // base too, so it is the clean checkout's five.
    let linked = Sandbox::new("linked-worktree");
    let calls = linked.assert_budget(&["context", "--json"], 0, 5);
    assert_eq!(
        verbs(&calls),
        verbs(&clean.git_processes(&["context", "--json"], 0).0)
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

/// `handover` finds the root, reads HEAD and its branch in one process, then
/// refreshes the live packet with the worktree it found.
#[test]
fn handover_costs_the_packet_and_one() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("dirty");
    let calls = sandbox.assert_budget(&["handover", "--next", "the lexer is done"], 0, 7);
    assert_eq!(
        calls[1],
        ["rev-parse", "HEAD", "--abbrev-ref", "HEAD"],
        "{calls:#?}"
    );
    let mut packet = verbs(&calls);
    packet.remove(1);
    assert_eq!(packet, PACKET, "{calls:#?}");
}
