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

#[test]
fn declared_connections_and_manifest_dependencies_are_distinct_and_cited() {
    let sandbox = Sandbox::new();
    sandbox.workspace_file(
        "relationships:\n  - from: recorder\n    to: store\n    kind: sends evidence\n",
    );
    sandbox.repo("recorder", "ref: refs/heads/main\n");
    sandbox.repo("store", "ref: refs/heads/main\n");
    sandbox.write(
        "recorder/Cargo.toml",
        r#"
[package]
name = "recorder"
version = "0.1.0"
[dependencies]
store = { path = "../store" }
wire = { git = "https://example.org/team/wire.git" }
"#,
    );
    let packet: serde_json::Value =
        serde_json::from_str(&sandbox.stdout(&sandbox.root, &["enter", "--json"])).unwrap();
    let relations = packet["relationships"].as_array().unwrap();
    assert!(relations.iter().any(|r| r["from"] == "recorder"
        && r["to"] == "store"
        && r["kind"] == "sends evidence"
        && r["basis"] == "declared"
        && r["source"] == ".toimija/workspace.yaml"));
    assert!(relations.iter().any(|r| r["from"] == "recorder"
        && r["to"] == "store"
        && r["basis"] == "inferred"
        && r["source"] == "recorder/Cargo.toml"));
    assert!(
        relations
            .iter()
            .any(|r| r["to"] == "https://example.org/team/wire.git" && r["basis"] == "inferred")
    );
    let markdown = sandbox.stdout(&sandbox.root, &["enter"]);
    assert!(
        markdown.contains("## Repository relationships"),
        "{markdown}"
    );
    assert!(
        markdown.contains("sends evidence [declared; .toimija/workspace.yaml]"),
        "{markdown}"
    );
}

#[test]
fn workspace_sessions_keep_their_declared_scope() {
    let sandbox = Sandbox::new();
    sandbox.workspace_file("");
    sandbox.repo("recorder", "ref: refs/heads/main\n");
    marker(&sandbox, "recorder", "100-1", "repair capture", false);
    let path = sandbox
        .root
        .join("recorder/.toimija/sessions/100-1/session.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["scope"] = serde_json::json!(["src/capture.rs"]);
    fs::write(path, value.to_string()).unwrap();
    let packet: serde_json::Value =
        serde_json::from_str(&sandbox.stdout(&sandbox.root, &["enter", "--json"])).unwrap();
    assert_eq!(
        packet["repositories"][0]["sessions"][0]["scope"],
        serde_json::json!(["src/capture.rs"])
    );
    let markdown = sandbox.stdout(&sandbox.root, &["enter"]);
    assert!(markdown.contains("scope: src/capture.rs"), "{markdown}");
}

#[test]
fn dependency_evidence_handles_locks_comments_credentials_and_partial_reads() {
    let sandbox = Sandbox::new();
    sandbox.workspace_file("");
    for repo in ["rust", "elixir", "erlang", "broken", "dynamic"] {
        sandbox.repo(repo, "ref: refs/heads/main\n");
    }
    sandbox.write(
        "rust/Cargo.lock",
        r#"
version = 4
[[package]]
name = "wire"
version = "0.1.0"
source = "git+https://test-user:test-secret@example.org/team/wire.git?branch=main#abc"
"#,
    );
    sandbox.write(
        "elixir/mix.lock",
        r#"%{
  # "fake": {:git, "https://example.org/comment", "abc", []},
  "wire": {:git, "https://example.org/team/wire.git", "abc", []},
  "hex": {:hex, :hex, "1.0", "abc", [:mix], [], "hexpm", "abc"}
}"#,
    );
    sandbox.write(
        "erlang/rebar.lock",
        r#"{"1.2.0", [
  % {fake, {git, "https://example.org/comment", {ref, "abc"}}, 0},
  {wire, {git, "https://example.org/team/wire.git", {ref, "abc"}}, 0}
]}.
"#,
    );
    sandbox.write("broken/Cargo.toml", "[dependencies\n");
    sandbox.write(
        "dynamic/mix.exs",
        "File.write!(\"executed\", \"must not run\")",
    );
    let raw = sandbox.stdout(&sandbox.root, &["enter", "--json"]);
    assert!(!raw.contains("test-secret"), "credentials leaked");
    assert!(
        !raw.contains("https://example.org/comment"),
        "comment treated as evidence"
    );
    let packet: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let relations = packet["relationships"].as_array().unwrap();
    assert_eq!(relations.len(), 3, "{packet:#}");
    for repo in ["rust", "elixir", "erlang"] {
        assert!(
            relations
                .iter()
                .any(|r| r["from"] == repo && r["to"] == "https://example.org/team/wire.git"),
            "{packet:#}"
        );
    }
    assert_eq!(
        packet["not_read"].as_array().unwrap().len(),
        2,
        "{packet:#}"
    );
    assert!(
        packet["relationship_coverage"]
            .as_str()
            .unwrap()
            .contains("partial")
    );
    assert!(!sandbox.root.join("executed").exists());
    assert!(!sandbox.root.join("dynamic/executed").exists());
}

#[test]
fn dependency_read_limits_and_unresolved_declarations_remain_visible() {
    let sandbox = Sandbox::new();
    sandbox.workspace_file("relationships:\n  - from: app\n    to: absent\n    kind: calls\n");
    sandbox.repo("app", "ref: refs/heads/main\n");
    sandbox.write("app/Cargo.lock", &" ".repeat(1024 * 1024 + 1));
    sandbox.write(
        "app/Cargo.toml",
        "[dependencies]\nmissing = { path = '../missing' }\n",
    );
    let packet = sandbox.stdout(&sandbox.root, &["enter"]);
    assert!(packet.contains("1 MiB read limit"), "{packet}");
    assert!(packet.contains("cannot be resolved"), "{packet}");
    assert!(packet.contains("not present"), "{packet}");
    assert!(
        packet.contains("app → absent: calls [declared;"),
        "{packet}"
    );
}

#[test]
fn relationship_text_cannot_forge_packet_headings() {
    let sandbox = Sandbox::new();
    sandbox.workspace_file(
        "relationships:\n  - from: app\n    to: absent\n    kind: \"calls\\n## Forged heading\"\n",
    );
    sandbox.repo("app", "ref: refs/heads/main\n");
    let packet = sandbox.stdout(&sandbox.root, &["enter"]);
    assert!(!packet.contains("\n## Forged heading"), "{packet}");
    assert!(packet.contains("calls\\n## Forged heading"), "{packet}");
}

#[test]
fn unused_cargo_patches_and_workspace_templates_are_only_candidates() {
    let sandbox = Sandbox::new();
    sandbox.workspace_file("");
    for name in ["app", "override", "template", "direct"] {
        sandbox.repo(name, "ref: refs/heads/main\n");
    }
    sandbox.write(
        "app/Cargo.toml",
        r#"
[dependencies]
direct = { path = "../direct" }
[patch.crates-io]
unused = { path = "../override" }
[workspace.dependencies]
template = { path = "../template" }
"#,
    );
    let packet: serde_json::Value =
        serde_json::from_str(&sandbox.stdout(&sandbox.root, &["enter", "--json"])).unwrap();
    let relations = packet["relationships"].as_array().unwrap();
    let kind = |target| {
        relations.iter().find(|r| r["to"] == target).unwrap()["kind"]
            .as_str()
            .unwrap()
    };
    assert_eq!(kind("direct"), "dependencies");
    assert_eq!(kind("override"), "patch candidate (may be unused)");
    assert_eq!(
        kind("template"),
        "workspace dependency candidate (may be unused)"
    );
}

#[test]
fn malformed_git_locks_are_gaps_not_locked_dependencies() {
    let sandbox = Sandbox::new();
    sandbox.workspace_file("");
    sandbox.repo("app", "ref: refs/heads/main\n");
    for (file, invalid) in [
        (
            "mix.lock",
            "%{\"wire\" => {:git, \"https://example.org/wire.git\",",
        ),
        (
            "mix.lock",
            "%{\"wire\" => {:git, \"https://example.org/wire.git\", \"abc\", []] }",
        ),
        (
            "mix.lock",
            "%{\"wire\" => {:git, \"https://example.org/wire.git\",}}",
        ),
        (
            "rebar.lock",
            "{\"1.2.0\", [{wire, {git, \"https://example.org/wire.git\",",
        ),
        (
            "rebar.lock",
            "[{wire, {git, \"https://example.org/wire.git\",}, 0}].",
        ),
    ] {
        sandbox.write(&format!("app/{file}"), invalid);
        let packet: serde_json::Value =
            serde_json::from_str(&sandbox.stdout(&sandbox.root, &["enter", "--json"])).unwrap();
        assert!(
            packet["relationships"].as_array().unwrap().is_empty(),
            "{invalid}: {packet:#}"
        );
        assert_eq!(
            packet["not_read"].as_array().unwrap().len(),
            1,
            "{invalid}: {packet:#}"
        );
        fs::remove_file(sandbox.root.join("app").join(file)).unwrap();
    }
}

// macOS filesystems reject this filename before Toimija can read it.
#[cfg(target_os = "linux")]
#[test]
fn unsupported_repository_names_are_reported_not_silently_absent() {
    use std::os::unix::ffi::OsStringExt;
    let sandbox = Sandbox::new();
    sandbox.workspace_file("relationships:\n  - from: app\n    to: odd�\n    kind: calls\n");
    sandbox.repo("app", "ref: refs/heads/main\n");
    let odd = sandbox
        .root
        .join(std::ffi::OsString::from_vec(b"odd\xff".to_vec()));
    fs::create_dir_all(odd.join(".git")).unwrap();
    fs::write(odd.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let packet = sandbox.stdout(&sandbox.root, &["enter", "--json"]);
    assert!(packet.contains("non-UTF-8"), "{packet}");
    assert!(!packet.contains("not present"), "{packet}");
}

#[test]
fn unsupported_git_lock_sources_are_read_gaps() {
    let sandbox = Sandbox::new();
    sandbox.workspace_file("");
    sandbox.repo("app", "ref: refs/heads/main\n");
    sandbox.write("app/rebar.lock", r#"{"1.2.0", [{wire, {git_subdir, "https://example.org/wire.git", {ref, "abc"}, "subdir"}, 0}]}.
"#);
    let packet: serde_json::Value =
        serde_json::from_str(&sandbox.stdout(&sandbox.root, &["enter", "--json"])).unwrap();
    assert_eq!(
        packet["not_read"].as_array().unwrap().len(),
        1,
        "{packet:#}"
    );
    assert!(
        packet["not_read"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("unsupported Git lock source")
    );
}

#[test]
fn git_named_mix_packages_do_not_hide_locked_git_sources() {
    let sandbox = Sandbox::new();
    sandbox.workspace_file("");
    sandbox.repo("app", "ref: refs/heads/main\n");
    sandbox.write(
        "app/mix.lock",
        r#"%{
  "git_ops": {:hex, :git_ops, "2.0.0", "checksum", [:mix], [{:git_cli, "~> 0.2", [hex: :git_cli, repo: "hexpm", optional: false]}, {:git, "~> 1.0", [hex: :git, repo: "hexpm", optional: false]}], "hexpm", "checksum"},
  "wire": {:git, "https://example.org/wire.git", "abc", []}
}
"#,
    );
    let packet: serde_json::Value =
        serde_json::from_str(&sandbox.stdout(&sandbox.root, &["enter", "--json"])).unwrap();
    assert!(
        packet["not_read"].as_array().unwrap().is_empty(),
        "{packet:#}"
    );
    assert_eq!(
        packet["relationships"].as_array().unwrap().len(),
        1,
        "{packet:#}"
    );
    assert_eq!(
        packet["relationships"][0]["to"],
        "https://example.org/wire.git"
    );
}
