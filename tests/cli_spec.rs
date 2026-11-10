//! A session can start from a spec: a file in the repository (an ADR, say) or
//! an issue reference. The spec is a fact the session declares, like its
//! intent. Toimija records it and shows it; it never fetches an issue and
//! never checks the work against the spec.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};

mod common;
use common::{git_available, run_git};

const SCRUBBED: &[&str] = &[
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "TOIMIJA_SESSION",
    "TOIMIJA_RUN",
    "XDG_STATE_HOME",
];

const ADR: &str =
    "# Login redirect\n\nStatus: accepted\n\n## Context\n\nUsers land on the wrong page.\n";

struct Sandbox {
    _temp: tempfile::TempDir,
    ceiling: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    out: PathBuf,
}

impl Sandbox {
    /// A repository with one commit on `main` that holds one ADR.
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let ceiling = temp.path().canonicalize().unwrap();
        let repo = ceiling.join("repo");
        let state = ceiling.join("state");
        let out = ceiling.join("out");
        for dir in [&repo, &state, &out] {
            fs::create_dir_all(dir).unwrap();
        }
        let sandbox = Self {
            _temp: temp,
            ceiling,
            repo,
            state,
            out,
        };
        run_git(&sandbox.repo, &["init", "-q", "-b", "main"]);
        sandbox.write("README.md", "# demo\n");
        sandbox.write("src/lib.rs", "pub fn login() {}\n");
        sandbox.write("docs/adr/0001-login-redirect.md", ADR);
        sandbox.write(
            ".toimija/repo.yaml",
            "repo:\n  name: demo\n  role: test fixture\nboundaries:\n  - stay inside fixture\n",
        );
        sandbox.write(
            ".toimija/.gitignore",
            "/current.md\n/current.json\n/sessions/\n/history.jsonl\n/history.lock\n/handover.json\n/tmp/\n",
        );
        run_git(&sandbox.repo, &["add", "-A"]);
        run_git(
            &sandbox.repo,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@localhost",
                "commit",
                "-q",
                "-m",
                "first",
            ],
        );
        sandbox
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.repo.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn toimija(&self, cwd: &Path) -> Command {
        let mut command = Command::cargo_bin("toimija").unwrap();
        command.current_dir(cwd);
        for var in SCRUBBED {
            command.env_remove(var);
        }
        command
            .env("GIT_CEILING_DIRECTORIES", &self.ceiling)
            .env("TOIMIJA_STATE_DIR", &self.state)
            .env("OUT", &self.out)
            .arg("-q");
        command
    }

    /// Run a stand-in agent named `claude` (a known agent, so it gets a
    /// launch prompt) under `toimija run <run_args> claude`. It saves its
    /// last argument, the prompt, then runs `body`, in which `$T` is toimija.
    fn run_claude(&self, cwd: &Path, run_args: &[&str], body: &str) -> assert_cmd::assert::Assert {
        let bin = self.ceiling.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let toimija = assert_cmd::cargo::cargo_bin("toimija");
        let script = format!(
            "#!/bin/sh\nT='{}'\nfor last; do :; done\nprintf '%s' \"$last\" > \"$OUT/prompt.txt\"\n{body}\nexit 0\n",
            toimija.display()
        );
        let path = bin.join("claude");
        fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let search = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        self.toimija(cwd)
            .env("PATH", search)
            .arg("run")
            .args(run_args)
            .arg("claude")
            .assert()
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.out.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
    }

    /// The one session marker a run left in the repository.
    fn marker(&self) -> serde_json::Value {
        let sessions = self.repo.join(".toimija/sessions");
        let mut markers: Vec<PathBuf> = fs::read_dir(&sessions)
            .unwrap()
            .map(|entry| entry.unwrap().path().join("session.json"))
            .filter(|path| path.is_file())
            .collect();
        assert_eq!(markers.len(), 1, "{markers:?}");
        serde_json::from_str(&fs::read_to_string(markers.remove(0)).unwrap()).unwrap()
    }
}

/// What the stand-in agent captures about its own session while it runs.
const CAPTURE: &str = "\"$T\" -q sessions > \"$OUT/sessions.txt\"\n\
     \"$T\" -q sessions --json > \"$OUT/sessions.json\"\n\
     \"$T\" -q graph > \"$OUT/graph.txt\" 2> \"$OUT/graph.err\"\n\
     \"$T\" -q graph --json > \"$OUT/graph.json\" 2>> \"$OUT/graph.err\"\n\
     cp \"$TOIMIJA_LIVE_JSON\" \"$OUT/packet.json\"";

#[test]
fn a_file_spec_is_kept_relative_to_the_root_and_shown_everywhere() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    // Given from a subfolder, relative to it.
    sandbox
        .run_claude(
            &sandbox.repo.join("src"),
            &[
                "--intent",
                "fix the redirect",
                "--spec",
                "../docs/adr/0001-login-redirect.md",
            ],
            CAPTURE,
        )
        .success();

    assert_eq!(sandbox.marker()["spec"], "docs/adr/0001-login-redirect.md");

    let prompt = sandbox.read("prompt.txt");
    assert!(
        prompt.contains(
            "## Spec\n- file: docs/adr/0001-login-redirect.md\n- title: Login redirect\n- status: accepted\n"
        ),
        "{prompt}"
    );
    assert!(
        prompt.contains("## Read first\n- docs/adr/0001-login-redirect.md (session spec)\n"),
        "{prompt}"
    );

    let sessions = sandbox.read("sessions.txt");
    assert!(
        sessions
            .contains("    intent: fix the redirect\n    spec: docs/adr/0001-login-redirect.md\n"),
        "{sessions}"
    );
    let json: serde_json::Value = serde_json::from_str(&sandbox.read("sessions.json")).unwrap();
    assert_eq!(
        json["sessions"][0]["spec"],
        "docs/adr/0001-login-redirect.md"
    );

    let graph = sandbox.read("graph.txt");
    assert!(
        graph.contains("\"fix the redirect\" [spec: docs/adr/0001-login-redirect.md]"),
        "{graph}\n{}",
        sandbox.read("graph.err")
    );
    assert!(
        sandbox
            .read("graph.json")
            .contains("\"spec\": \"docs/adr/0001-login-redirect.md\""),
        "{}",
        sandbox.read("graph.json")
    );

    let packet: serde_json::Value = serde_json::from_str(&sandbox.read("packet.json")).unwrap();
    assert_eq!(
        packet["session"]["spec"],
        serde_json::json!({
            "reference": "docs/adr/0001-login-redirect.md",
            "kind": "file",
            "title": "Login redirect",
            "status": "accepted",
        })
    );
    assert_eq!(
        packet["read_first"][0],
        serde_json::json!({"path": "docs/adr/0001-login-redirect.md", "reason": "session spec"})
    );

    // Once the session has ended, the repository-level packet is no longer
    // its packet: it holds no spec, and Read first does not point at one.
    for name in ["current.md", "current.json"] {
        let summary = fs::read_to_string(sandbox.repo.join(".toimija").join(name)).unwrap();
        assert!(
            !summary.contains("session spec") && !summary.contains("Spec"),
            "{name}: {summary}"
        );
        assert!(
            !summary.contains("0001-login-redirect"),
            "{name}: {summary}"
        );
    }
}

#[test]
fn without_a_spec_nothing_says_spec() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    sandbox
        .run_claude(&sandbox.repo, &["--intent", "fix the redirect"], CAPTURE)
        .success();

    assert!(sandbox.marker().get("spec").is_none());
    for name in [
        "prompt.txt",
        "sessions.txt",
        "sessions.json",
        "graph.txt",
        "graph.json",
        "packet.json",
    ] {
        let text = sandbox.read(name);
        assert!(
            !text.contains("spec") && !text.contains("Spec"),
            "{name}: {text}"
        );
    }
}

#[test]
fn a_spec_file_that_is_missing_or_outside_the_repository_stops_the_launch() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    sandbox
        .run_claude(
            &sandbox.repo,
            &["--spec", "docs/adr/0009-nothing.md"],
            "touch \"$OUT/launched\"",
        )
        .failure()
        .stderr(predicate::str::contains("docs/adr/0009-nothing.md"))
        .stderr(predicate::str::contains("does not exist"));

    fs::write(sandbox.ceiling.join("elsewhere.md"), "# Elsewhere\n").unwrap();
    sandbox
        .run_claude(
            &sandbox.repo,
            &["--spec", "../elsewhere.md"],
            "touch \"$OUT/launched\"",
        )
        .failure()
        .stderr(predicate::str::contains("outside the repository"));

    assert!(!sandbox.out.join("launched").exists());
    assert!(!sandbox.repo.join(".toimija/sessions").exists());
}

#[test]
fn an_issue_reference_is_kept_as_given_and_never_read() {
    if !git_available() {
        return;
    }
    for reference in ["#123", "https://tracker.example.invalid/demo/issues/123"] {
        let sandbox = Sandbox::new();
        sandbox
            .run_claude(&sandbox.repo, &["--spec", reference], CAPTURE)
            .success();
        assert_eq!(sandbox.marker()["spec"], reference);
        let prompt = sandbox.read("prompt.txt");
        assert!(
            prompt.contains(&format!(
                "## Spec\n- reference: {reference}\n- an issue reference: toimija does not read issues\n"
            )),
            "{prompt}"
        );
        assert!(!prompt.contains("(session spec)"), "{prompt}");
        let packet: serde_json::Value = serde_json::from_str(&sandbox.read("packet.json")).unwrap();
        assert_eq!(
            packet["session"]["spec"],
            serde_json::json!({"reference": reference, "kind": "issue"})
        );
    }
}

#[test]
fn a_title_or_status_the_file_does_not_state_is_not_guessed() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    sandbox.write(
        "docs/notes.md",
        "Some notes.\n\n## Status\n\nmostly done\n\nstatus: lowercase is another word\n",
    );
    sandbox
        .run_claude(&sandbox.repo, &["--spec", "docs/notes.md"], CAPTURE)
        .success();
    let prompt = sandbox.read("prompt.txt");
    assert!(
        prompt.contains(
            "## Spec\n- file: docs/notes.md\n- title: not stated\n- status: not stated\n"
        ),
        "{prompt}"
    );
}

#[test]
fn intent_sets_a_spec_mid_session_with_or_without_a_new_intent() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    sandbox
        .run_claude(
            &sandbox.repo,
            &["--intent", "look around"],
            "\"$T\" -q intent --spec docs/adr/0001-login-redirect.md > \"$OUT/first.txt\"\n\
             \"$T\" -q sessions > \"$OUT/after-first.txt\"\n\
             \"$T\" -q intent 'fix the redirect' --spec '#7' > \"$OUT/second.txt\"\n\
             \"$T\" -q sessions > \"$OUT/after-second.txt\"\n\
             \"$T\" -q intent --spec docs/adr/0009-nothing.md 2> \"$OUT/missing.err\"\n\
             \"$T\" -q intent 2> \"$OUT/nothing.err\"",
        )
        .success();

    // A spec alone leaves the intent as it was.
    let first = sandbox.read("after-first.txt");
    assert!(
        first.contains("    intent: look around\n    spec: docs/adr/0001-login-redirect.md\n"),
        "{first}"
    );
    assert!(
        sandbox
            .read("first.txt")
            .contains("spec — docs/adr/0001-login-redirect.md")
    );
    // Both at once.
    let second = sandbox.read("after-second.txt");
    assert!(
        second.contains("    intent: fix the redirect\n    spec: #7\n"),
        "{second}"
    );
    // A missing file changes nothing and is named.
    assert!(
        sandbox
            .read("missing.err")
            .contains("docs/adr/0009-nothing.md")
    );
    assert_eq!(sandbox.marker()["spec"], "#7");
    // Neither a text nor a spec is a usage error.
    assert!(!sandbox.read("nothing.err").is_empty());
}

fn listing(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            out.push(path.strip_prefix(root).unwrap().display().to_string());
            if path.is_dir() {
                walk(&path, root, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn spec_new_writes_the_next_numbered_file_and_nothing_else() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    let before = listing(&sandbox.repo);
    let output = sandbox
        .toimija(&sandbox.repo.join("src"))
        .args(["spec", "new", "Rate limit the Login API!"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "docs/adr/0002-rate-limit-the-login-api.md"
    );
    assert_eq!(
        fs::read_to_string(
            sandbox
                .repo
                .join("docs/adr/0002-rate-limit-the-login-api.md")
        )
        .unwrap(),
        "# Rate limit the Login API!\n\nStatus: proposed\n\n## Context\n\n## Decision\n\n## Consequences\n"
    );
    let mut expected = before;
    expected.push("docs/adr/0002-rate-limit-the-login-api.md".to_string());
    expected.sort();
    assert_eq!(listing(&sandbox.repo), expected, "spec new wrote more");

    // The number follows the highest one there, not the count.
    sandbox.write("docs/adr/0041-something.md", "# Something\n");
    let next = sandbox
        .toimija(&sandbox.repo)
        .args(["spec", "new", "After a gap"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&next.stdout).trim(),
        "docs/adr/0042-after-a-gap.md"
    );

    // A title with nothing to name a file by is refused.
    sandbox
        .toimija(&sandbox.repo)
        .args(["spec", "new", "!!!"])
        .assert()
        .failure();
}

#[test]
fn spec_new_and_list_use_the_folder_repo_yaml_names() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    sandbox.write(
        ".toimija/repo.yaml",
        "repo:\n  name: demo\n  role: test fixture\nspecs: decisions\nboundaries:\n  - stay inside fixture\n",
    );
    let output = sandbox
        .toimija(&sandbox.repo)
        .args(["spec", "new", "First decision"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "decisions/0001-first-decision.md"
    );
    sandbox.write("decisions/0002-old.md", "# An old one\n\nNo status here.\n");
    sandbox.write("decisions/README.md", "# not a spec\n");

    let list = sandbox
        .toimija(&sandbox.repo)
        .args(["spec", "list"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&list.stdout),
        "decisions/0001-first-decision.md  proposed  First decision\n\
         decisions/0002-old.md  not stated  An old one\n"
    );
}

#[test]
fn spec_list_in_a_repository_with_no_spec_folder_says_so() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    fs::remove_dir_all(sandbox.repo.join("docs")).unwrap();
    sandbox
        .toimija(&sandbox.repo)
        .args(["spec", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No specs in docs/adr"));
}

#[test]
fn the_workspace_packet_shows_a_session_spec_beside_its_intent() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    let work = sandbox.ceiling.join("work");
    fs::create_dir_all(work.join(".toimija")).unwrap();
    fs::write(work.join(".toimija/workspace.yaml"), "").unwrap();
    fs::create_dir_all(work.join("alpha/.git")).unwrap();
    fs::write(work.join("alpha/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let session = work.join("alpha/.toimija/sessions/100-1");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("session.json"),
        serde_json::json!({
            "id": "100-1",
            "pid": std::process::id(),
            "agent": ["codex"],
            "worktree": work.join("alpha"),
            "branch": "main",
            "intent": "fix the redirect",
            "spec": "docs/adr/0001-login-redirect.md",
            "started_at": now,
            "last_seen_at": now,
            "status": "active",
            "closed_at": null,
        })
        .to_string(),
    )
    .unwrap();

    let output = sandbox.toimija(&work).arg("context").output().unwrap();
    let packet = String::from_utf8_lossy(&output.stdout);
    assert!(
        packet.contains("codex (live): fix the redirect [spec: docs/adr/0001-login-redirect.md]"),
        "{packet}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = sandbox
        .toimija(&work)
        .args(["context", "--json"])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        json["repositories"][0]["sessions"][0]["spec"],
        "docs/adr/0001-login-redirect.md"
    );
}
