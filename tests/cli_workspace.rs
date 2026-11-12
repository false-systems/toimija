//! A workspace: a folder that holds repositories and describes itself with
//! `.toimija/workspace.yaml`. Outside any repository, toimija finds the
//! nearest such file above the current directory and reports the folder from
//! files alone. Inside a repository, and in a folder with no workspace file,
//! nothing changes.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};

mod common;
use common::{git_available, run_git};

/// Everything that could make a folder look like it is inside a repo, or
/// point toimija at one.
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

struct Sandbox {
    _temp: tempfile::TempDir,
    /// Git may not look above this folder for a repository.
    ceiling: PathBuf,
    /// The workspace folder.
    root: PathBuf,
    state: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let ceiling = temp.path().canonicalize().unwrap();
        let root = ceiling.join("work");
        let state = ceiling.join("state");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&state).unwrap();
        Self {
            _temp: temp,
            ceiling,
            root,
            state,
        }
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
            .arg("-q");
        command
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn workspace_file(&self, contents: &str) {
        self.write(".toimija/workspace.yaml", contents);
    }

    /// A repository made of files alone: a `.git` directory with a `HEAD`.
    fn repo(&self, name: &str, head: &str) {
        self.write(&format!("{name}/.git/HEAD"), head);
    }

    fn stdout(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.toimija(cwd).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "toimija {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

/// Three repositories and what a workspace must sort out about them: a plain
/// one on a branch, a linked worktree (`.git` is a file) with a detached HEAD
/// and a waiting handover, and one whose HEAD cannot be read.
fn three_repositories() -> Sandbox {
    let sandbox = Sandbox::new();
    sandbox.workspace_file(
        "role: the services of one product\nread_first:\n  - NOTES.md\n  - docs/missing.md\n",
    );
    sandbox.write("NOTES.md", "# notes\n");
    sandbox.write("loose-file.txt", "not a repository\n");
    sandbox.write("plain-folder/readme.txt", "not a repository\n");

    sandbox.repo("alpha", "ref: refs/heads/main\n");
    sandbox.write(
        "alpha/.toimija/repo.yaml",
        "repo:\n  name: alpha\n  role: service that signs users in\n",
    );

    // A linked worktree: `.git` is a file that names the real git dir.
    sandbox.write("beta/.git", "gitdir: ../store/beta-git\n");
    sandbox.write(
        "store/beta-git/HEAD",
        "0123456789abcdef0123456789abcdef01234567\n",
    );
    sandbox.write(
        "beta/.toimija/handover.json",
        r#"{"written_at": 1794000000, "next": "the parser is done; start at src/lexer.rs\nand read the tests"}"#,
    );

    sandbox.repo("gamma", "");
    sandbox.write("gamma/.toimija/repo.yaml", "repo: [not, a, mapping\n");
    sandbox
}

#[test]
fn enter_in_a_workspace_prints_the_workspace_packet() {
    if !git_available() {
        return;
    }
    let sandbox = three_repositories();
    let packet = sandbox.stdout(&sandbox.root, &["enter"]);

    assert!(packet.starts_with("# Toimija Workspace\n"), "{packet}");
    assert!(
        packet.contains(&format!("- folder: {}\n", sandbox.root.display())),
        "{packet}"
    );
    assert!(
        packet.contains("- role: the services of one product\n"),
        "{packet}"
    );
    assert!(
        packet.contains("## Read first\n- NOTES.md\n- docs/missing.md (missing)\n"),
        "{packet}"
    );

    // The repository with a waiting handover comes first, then by name.
    let beta = packet.find("\n- beta\n").expect("beta listed");
    let alpha = packet.find("\n- alpha\n").expect("alpha listed");
    let gamma = packet.find("\n- gamma\n").expect("gamma listed");
    assert!(beta < alpha && alpha < gamma, "{packet}");
    assert!(
        packet.contains(
            "- alpha\n  - role: service that signs users in\n  - branch: main\n  - sessions: none\n  - handover: none\n"
        ),
        "{packet}"
    );
    assert!(
        packet.contains(
            "- beta\n  - role: not set\n  - branch: detached at 0123456\n  - sessions: none\n  - handover: the parser is done; start at src/lexer.rs\n"
        ),
        "{packet}"
    );
    assert!(
        packet.contains("- gamma\n  - role: unknown\n  - branch: unknown\n"),
        "{packet}"
    );
    // A folder without `.git` and a loose file are not repositories.
    assert!(!packet.contains("plain-folder"), "{packet}");
    assert!(!packet.contains("loose-file"), "{packet}");
    assert!(!packet.contains("- store\n"), "{packet}");

    // Nothing unreadable is dropped: each is named with its reason.
    let not_read = &packet[packet.find("## Not read\n").expect("a Not read section")..];
    assert!(not_read.contains("gamma: branch"), "{packet}");
    assert!(not_read.contains("gamma: role"), "{packet}");

    assert!(
        packet.contains("run `toimija enter` inside a repository"),
        "{packet}"
    );
}

#[test]
fn the_json_packet_names_its_schema_and_holds_the_same_facts() {
    if !git_available() {
        return;
    }
    let sandbox = three_repositories();
    let packet: serde_json::Value =
        serde_json::from_str(&sandbox.stdout(&sandbox.root, &["enter", "--json"])).unwrap();

    assert_eq!(packet["schema"], "toimija-workspace.v1");
    assert_eq!(
        packet["workspace"]["folder"],
        sandbox.root.display().to_string()
    );
    assert_eq!(packet["workspace"]["role"], "the services of one product");
    assert_eq!(
        packet["workspace"]["read_first"],
        serde_json::json!([
            {"path": "NOTES.md", "missing": false},
            {"path": "docs/missing.md", "missing": true},
        ])
    );
    let names: Vec<&str> = packet["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|repository| repository["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["beta", "alpha", "gamma"]);
    let beta = &packet["repositories"][0];
    assert_eq!(beta["branch"], "detached at 0123456");
    assert_eq!(beta["role"], serde_json::Value::Null);
    assert_eq!(
        beta["handover"]["first_line"],
        "the parser is done; start at src/lexer.rs"
    );
    let gamma = &packet["repositories"][2];
    assert_eq!(gamma["branch"], serde_json::Value::Null);
    assert_eq!(packet["sessions"], serde_json::json!([]));
    assert_eq!(
        packet["not_read"].as_array().unwrap().len(),
        2,
        "{packet:#}"
    );
}

#[test]
fn the_nearest_workspace_file_above_the_current_directory_wins() {
    if !git_available() {
        return;
    }
    let sandbox = three_repositories();
    // From a plain subfolder, the walk goes up to the workspace.
    let below = sandbox.root.join("plain-folder");
    assert!(
        sandbox
            .stdout(&below, &["enter"])
            .contains("- role: the services of one product\n")
    );

    // A workspace file nearer than that one wins.
    fs::create_dir_all(below.join(".toimija")).unwrap();
    fs::write(
        below.join(".toimija/workspace.yaml"),
        "role: the inner one\n",
    )
    .unwrap();
    let inner = sandbox.stdout(&below, &["enter"]);
    assert!(inner.contains("- role: the inner one\n"), "{inner}");
    assert!(
        inner.contains(&format!("- folder: {}\n", below.display())),
        "{inner}"
    );
}

#[test]
fn inside_a_repository_the_repository_wins_over_a_workspace_above_it() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    sandbox.workspace_file("role: the workspace\n");
    let repo = sandbox.root.join("real");
    fs::create_dir_all(&repo).unwrap();
    run_git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("README.md"), "# real\n").unwrap();
    run_git(&repo, &["add", "README.md"]);
    run_git(
        &repo,
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

    let packet = sandbox.stdout(&repo, &["enter"]);
    assert!(packet.starts_with("# Toimija Context\n"), "{packet}");
    assert!(!packet.contains("Toimija Workspace"), "{packet}");

    // And the real repository shows up in the workspace, on its branch.
    let workspace = sandbox.stdout(&sandbox.root, &["enter"]);
    assert!(
        workspace.contains("- real\n  - role: not set\n  - branch: main\n"),
        "{workspace}"
    );
}

#[test]
fn outside_a_repository_and_a_workspace_enter_fails_and_names_the_way_in() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    sandbox
        .toimija(&sandbox.root)
        .arg("enter")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a Git repository"))
        .stderr(predicate::str::contains(".toimija/workspace.yaml"));
}

#[test]
fn an_empty_workspace_file_is_a_workspace_and_an_unknown_field_is_named() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    sandbox.workspace_file("");
    let packet = sandbox.stdout(&sandbox.root, &["enter"]);
    assert!(packet.contains("- role: not set\n"), "{packet}");
    assert!(packet.contains("## Read first\n- none\n"), "{packet}");
    assert!(packet.contains("## Repositories\n- none\n"), "{packet}");

    sandbox.workspace_file("role: x\nrepositories:\n  - a\n");
    sandbox
        .toimija(&sandbox.root)
        .arg("enter")
        .assert()
        .failure()
        .stderr(predicate::str::contains("workspace.yaml"))
        .stderr(predicate::str::contains("repositories"));
}

fn listing(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
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
fn enter_in_a_workspace_prints_the_packet_and_writes_nothing() {
    if !git_available() {
        return;
    }
    let sandbox = three_repositories();
    let before = listing(&sandbox.ceiling);
    let entered = sandbox.stdout(&sandbox.root, &["enter"]);
    assert!(entered.contains("# Toimija Workspace\n"), "{entered}");
    assert_eq!(
        entered,
        sandbox.stdout(&sandbox.root, &["enter"]),
        "the packet is worked out the same way each time"
    );
    let json: serde_json::Value =
        serde_json::from_str(&sandbox.stdout(&sandbox.root, &["enter", "--json"])).unwrap();
    assert_eq!(json["schema"], "toimija-workspace.v1");
    assert_eq!(before, listing(&sandbox.ceiling), "enter wrote something");
}

/// A stand-in agent named `claude` (a known agent, so it gets a launch
/// prompt): it saves its last argument, the prompt, and what `toimija
/// enter` prints while it runs.
fn fake_claude(sandbox: &Sandbox) -> PathBuf {
    let bin = sandbox.ceiling.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let toimija = assert_cmd::cargo::cargo_bin("toimija");
    let script = format!(
        "#!/bin/sh\nfor last; do :; done\nprintf '%s' \"$last\" > \"$OUT/prompt.txt\"\n'{}' -q enter > \"$OUT/during.md\" 2> \"$OUT/during.err\"\nexit 0\n",
        toimija.display()
    );
    let path = bin.join("claude");
    fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

fn run_fake_claude(sandbox: &Sandbox, cwd: &Path) -> PathBuf {
    let bin = fake_claude(sandbox);
    let out = sandbox.ceiling.join("out");
    fs::create_dir_all(&out).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    sandbox
        .toimija(cwd)
        .env("PATH", path)
        .env("OUT", &out)
        .args(["run", "--intent", "sweep the services", "claude"])
        .assert()
        .success();
    out
}

#[test]
fn run_in_a_workspace_puts_the_workspace_packet_in_the_launch_prompt() {
    if !git_available() {
        return;
    }
    let sandbox = three_repositories();
    let before = listing(&sandbox.root);
    let out = run_fake_claude(&sandbox, &sandbox.root);

    let prompt = fs::read_to_string(out.join("prompt.txt")).unwrap();
    assert!(prompt.contains("# Toimija Workspace\n"), "{prompt}");
    assert!(
        prompt.contains("- role: the services of one product\n"),
        "{prompt}"
    );
    assert!(prompt.contains("- alpha\n"), "{prompt}");
    assert!(
        !prompt.contains("There is no \nsession packet")
            && !prompt.contains("this folder has none"),
        "{prompt}"
    );

    // The session is open in the workspace folder while the agent runs, and
    // the packet built then says so.
    let during = fs::read_to_string(out.join("during.md")).unwrap();
    let sessions = &during[during
        .find("## Sessions in this folder\n")
        .expect("a sessions section")..];
    assert!(
        sessions.contains("claude (live): sweep the services"),
        "{during}"
    );

    // Recorded in the state directory, and nothing written beside the work.
    assert_eq!(before, listing(&sandbox.root), "run wrote in the workspace");
    assert!(sandbox.state.join(".toimija/sessions").is_dir());
}

#[test]
fn run_in_a_folder_with_no_workspace_file_is_as_it_was() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    sandbox.repo("alpha", "ref: refs/heads/main\n");
    let out = run_fake_claude(&sandbox, &sandbox.root);

    let prompt = fs::read_to_string(out.join("prompt.txt")).unwrap();
    assert_eq!(
        prompt,
        format!(
            "You are in a toimija session outside any Git repository ({}). There is no \
             session packet: toimija compiles Git context, and this folder has none. \
             When work is assigned, declare why you are here and what you will touch \
             (`toimija intent \"<why>\" --scope <path>`); `toimija sessions --outside-repos` \
             lists you. With no concrete task, acknowledge and wait instead of running \
             discovery or bookkeeping tools. Once a repository is selected, run `toimija enter` \
             inside it once and use that workset instead of scanning the workspace.\n",
            sandbox.root.display()
        )
    );
    // And `enter` there still fails.
    let error = fs::read_to_string(out.join("during.err")).unwrap();
    assert!(error.contains("not a Git repository"), "{error}");
    assert!(!sandbox.root.join(".toimija").exists());
}

/// Every git process `toimija enter` starts in `cwd`, by Git's own trace.
fn git_processes(sandbox: &Sandbox, cwd: &Path) -> Vec<Vec<String>> {
    let trace = sandbox.ceiling.join("trace.json");
    let _ = fs::remove_file(&trace);
    let output = sandbox
        .toimija(cwd)
        .env("GIT_TRACE2_EVENT", &trace)
        .arg("enter")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    fs::read_to_string(&trace)
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
        .collect()
}

/// The workspace packet is built from files: no git process per repository,
/// and none for the packet at all. The one process `enter` starts is the
/// question every command asks first, "is this folder inside a repository?",
/// and its count does not grow with the number of repositories.
#[test]
fn the_workspace_packet_starts_no_git_process_however_many_repositories() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    sandbox.workspace_file("role: many\n");
    let real = |name: &str| {
        let repo = sandbox.root.join(name);
        fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
    };
    for name in ["a", "b", "c"] {
        real(name);
    }
    let three = git_processes(&sandbox, &sandbox.root);
    for name in ["d", "e", "f", "g"] {
        real(name);
    }
    let seven = git_processes(&sandbox, &sandbox.root);

    assert_eq!(three.len(), 1, "{three:#?}");
    assert_eq!(seven, three, "the count grew with the repositories");
    assert!(
        three[0].iter().any(|arg| arg == "rev-parse"),
        "the one process is the discovery: {three:#?}"
    );
    // Positive control: the trace sees git when toimija does start it.
    let inside = git_processes(&sandbox, &sandbox.root.join("a"));
    assert!(inside.len() > 1, "{inside:#?}");
}

/// A session marker as `run` writes it in a repository, for this test
/// process's own pid: alive, or, `ended`, with a start this process never had.
fn marker(sandbox: &Sandbox, repo: &str, id: &str, intent: &str, ended: bool) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    sandbox.write(
        &format!("{repo}/.toimija/sessions/{id}/session.json"),
        &serde_json::json!({
            "id": id,
            "pid": std::process::id(),
            "agent": ["codex", "--model", "x"],
            "worktree": sandbox.root.join(repo),
            "branch": "main",
            "intent": intent,
            "started_at": now,
            "last_seen_at": now,
            "pid_start": if ended { Some(1) } else { None },
        })
        .to_string(),
    );
}

#[test]
fn a_repository_with_an_open_session_is_listed_first_with_that_session() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new();
    sandbox.workspace_file("");
    sandbox.repo("alpha", "ref: refs/heads/main\n");
    sandbox.repo("zeta", "ref: refs/heads/fix/redirect\n");
    marker(&sandbox, "zeta", "100-1", "fix the redirect", false);
    marker(&sandbox, "alpha", "100-2", "done long ago", true);
    // A marker that cannot be read is named, not dropped.
    sandbox.write("alpha/.toimija/sessions/100-3/session.json", "not json");

    let packet = sandbox.stdout(&sandbox.root, &["enter"]);
    let zeta = packet.find("\n- zeta\n").unwrap();
    let alpha = packet.find("\n- alpha\n").unwrap();
    assert!(zeta < alpha, "{packet}");
    assert!(
        packet.contains(
            "- zeta\n  - role: not set\n  - branch: fix/redirect\n  - session: codex (live): fix the redirect\n  - handover: none\n"
        ),
        "{packet}"
    );
    assert!(
        packet.contains("- alpha\n  - role: not set\n  - branch: main\n  - sessions: none\n"),
        "{packet}"
    );
    assert!(!packet.contains("done long ago"), "{packet}");
    let not_read = &packet[packet.find("## Not read\n").expect("a Not read section")..];
    assert!(not_read.contains("alpha: sessions"), "{packet}");
}
