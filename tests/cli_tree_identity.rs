//! Toimija's State identity is faija's tree-identity.v1.
//!
//! The tree in a snapshot, a repository point and a session packet is the one
//! faija-terms' canonical procedure yields: the work content, leaving out the
//! generated namespaces `.toimija`, `.teko` and `.sykli`, with
//! `.toimija/repo.yaml` forced in. These tests run that procedure here, from
//! the vendored crate and with real git, and hold Toimija's commands to it.
//!
//! They also hold the pairing: a repository point is a tree *and* the token of
//! the state it was written from. A worktree that moves while the tree is being
//! written must never come out as a tree from one moment beside a token from
//! another.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use faija_terms::{Step, TreeIdentityAlgorithm};
use tempfile::TempDir;

mod common;
use common::{git_at, git_available, run_git};

/// `.sykli/` hidden by the ignore rules, and `.sykli/` in plain view: the
/// identity must not care either way.
const IGNORE_RULES: [&str; 2] = [".toimija/\n.teko/\n.sykli/\n", ".toimija/\n.teko/\n"];

/// A repository holding every kind of content the identity must sort:
/// committed source, an untracked source file, `.toimija/repo.yaml` that the
/// ignore rules hide and nothing tracks, a receipt committed under `.sykli/`
/// (a repository might), an untracked one beside it, and `.teko/` state.
fn fixture(gitignore: &str) -> TempDir {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path();
    run_git(path, &["init", "-q"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    write(path, ".gitignore", gitignore);
    write(path, "src/main.rs", "fn main() {}\n");
    write(path, ".toimija/repo.yaml", "repo:\n  name: fixture\n");
    write(path, ".sykli/tracked.json", "{\"outcome\":\"passed\"}\n");
    run_git(path, &["add", "-A"]);
    run_git(path, &["add", "-f", "--", ".sykli/tracked.json"]);
    run_git(path, &["commit", "-q", "-m", "init"]);
    write(path, "src/new.rs", "pub fn g() {}\n");
    write(path, ".sykli/receipt.json", "{\"outcome\":\"passed\"}\n");
    write(path, ".teko/state.json", "{\"open\":true}\n");
    temp
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn git_out(root: &Path, args: &[&str], index: Option<&Path>) -> Result<String, String> {
    let mut command = git_at(root);
    command.args(args);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = command.output().unwrap();
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// faija's canonical procedure, run step by step exactly as faija-terms
/// returns it, against a scratch index outside the repository. Independent of
/// Toimija's code: this is what Toimija's tree must equal.
fn faija_tree(root: &Path) -> String {
    let scratch = tempfile::tempdir().unwrap();
    let index = scratch.path().join("faija.index");
    let index = Some(index.as_path());
    let mut tree = None;
    for step in TreeIdentityAlgorithm::V1.plan() {
        match step {
            Step::SeedFromHead => {
                // Tolerated: an unborn HEAD has no tracked files.
                let _ = git_out(root, &["read-tree", "HEAD"], index);
            }
            Step::OverlayWorktree { excludes } => {
                let mut args = vec!["add", "-A", "--"];
                args.extend(excludes.iter().map(String::as_str));
                git_out(root, &args, index).expect("overlay the worktree");
            }
            Step::DropNamespaces { paths } => {
                let mut args = vec!["rm", "--cached", "-r", "-q", "--ignore-unmatch"];
                args.extend(paths.iter().map(String::as_str));
                git_out(root, &args, index).expect("drop the namespaces");
            }
            Step::ForceAddConfig { path } => {
                if root.join(&path).is_file() {
                    git_out(root, &["add", "-f", "--", &path], index).expect("force-add config");
                }
            }
            Step::WriteTree => tree = Some(git_out(root, &["write-tree"], index).unwrap()),
        }
    }
    tree.expect("the plan ends in write-tree")
}

fn tree_paths(root: &Path, tree: &str) -> Vec<String> {
    git_out(root, &["ls-tree", "-r", "--name-only", tree], None)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn toimija(root: &Path) -> Command {
    let mut command = Command::cargo_bin("toimija").unwrap();
    command.current_dir(root).env_remove("TOIMIJA_SESSION");
    command
}

fn json(root: &Path, args: &[&str]) -> serde_json::Value {
    let assert = toimija(root).args(args).assert().success();
    serde_json::from_slice(&assert.get_output().stdout).expect("valid JSON")
}

fn snapshot(root: &Path) -> serde_json::Value {
    json(root, &["snapshot", "--json"])
}

fn point(root: &Path) -> serde_json::Value {
    json(root, &["repository-point", "--json"])
}

fn snapshot_tree(root: &Path) -> String {
    snapshot(root)["tree"].as_str().unwrap().to_string()
}

fn point_tree(root: &Path) -> String {
    point(root)["repository"]["tree_oid"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn the_tree_is_faija_tree_identity_v1_whatever_sykli_and_the_ignore_rules_hold() {
    if !git_available() {
        return;
    }
    for rules in IGNORE_RULES {
        let temp = fixture(rules);
        let root = temp.path();
        let expected = faija_tree(root);

        // The fixture exercises what it claims to.
        let paths = tree_paths(root, &expected);
        assert!(
            paths.contains(&".toimija/repo.yaml".to_string()),
            "{paths:?}"
        );
        assert!(paths.contains(&"src/new.rs".to_string()), "{paths:?}");
        assert!(
            !paths
                .iter()
                .any(|path| path.starts_with(".sykli/") || path.starts_with(".teko/")),
            "{paths:?}"
        );

        assert_eq!(snapshot_tree(root), expected, "snapshot, rules {rules:?}");
        assert_eq!(
            point_tree(root),
            expected,
            "repository-point, rules {rules:?}"
        );
    }
}

#[test]
fn sykli_state_never_moves_the_tree_but_work_content_does() {
    if !git_available() {
        return;
    }
    for rules in IGNORE_RULES {
        let temp = fixture(rules);
        let root = temp.path();
        let before = snapshot_tree(root);
        assert_eq!(point_tree(root), before);

        // sykli runs again: the committed receipt is rewritten, another lands.
        write(root, ".sykli/tracked.json", "{\"outcome\":\"failed\"}\n");
        write(
            root,
            ".sykli/receipts/rcpt_2.json",
            "{\"outcome\":\"passed\"}\n",
        );
        assert_eq!(snapshot_tree(root), before, "snapshot, rules {rules:?}");
        assert_eq!(
            point_tree(root),
            before,
            "repository-point, rules {rules:?}"
        );

        // Configuration is work content even while ignored, and so is source.
        write(root, ".toimija/repo.yaml", "repo:\n  name: renamed\n");
        let configured = snapshot_tree(root);
        assert_ne!(configured, before, "a config change must move the tree");
        write(root, "src/main.rs", "fn main() { println!(\"hi\"); }\n");
        assert_ne!(
            snapshot_tree(root),
            configured,
            "a source change must move it"
        );
    }
}

/// The tree in every output names the algorithm that produced it, so a reader
/// never has to guess which rules a stored tree was computed under.
#[test]
fn points_snapshots_and_packets_name_their_tree_algorithm() {
    if !git_available() {
        return;
    }
    let temp = fixture(IGNORE_RULES[1]);
    let root = temp.path();
    let v1 = TreeIdentityAlgorithm::V1.id();

    assert_eq!(point(root)["extensions"]["tree_algorithm"], v1);
    assert_eq!(snapshot(root)["tree_algorithm"], v1);

    // Inside a session: the point file and the sealed packet's execution
    // context, whose tree is the same canonical tree.
    let out = tempfile::tempdir().unwrap();
    toimija(root)
        .env("OUT", out.path())
        .args([
            "run",
            "--intent",
            "test",
            "--",
            "sh",
            "-c",
            "cp \"$FALSE_SYSTEMS_REPOSITORY_POINT_PATH\" \"$OUT/point.json\" && \
             cp \"$TOIMIJA_LIVE_JSON\" \"$OUT/packet.json\"",
        ])
        .assert()
        .success();
    let read = |name: &str| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(out.path().join(name)).unwrap()).unwrap()
    };
    let session_point = read("point.json");
    let execution = &read("packet.json")["session"]["execution"];
    assert_eq!(session_point["extensions"]["tree_algorithm"], v1);
    assert_eq!(execution["extensions"]["tree_algorithm"], v1);
    assert_eq!(execution["repository"]["tree_oid"], faija_tree(root));
}

/// A lease taken before Toimija computed tree-identity.v1 recorded no
/// algorithm. Its tree is what the old procedure wrote, and it stays exactly
/// that: listed as such, never recomputed or relabelled, and still releasable.
#[test]
fn a_lease_from_before_tree_identity_v1_keeps_its_tree_and_reads_as_legacy() {
    if !git_available() {
        return;
    }
    let temp = fixture(IGNORE_RULES[1]);
    let root = temp.path();
    // The old procedure kept `.sykli/`; HEAD's tree, which holds the committed
    // receipt, stands in for such a tree.
    let legacy_tree = git_out(root, &["rev-parse", "HEAD^{tree}"], None).unwrap();
    run_git(
        root,
        &[
            "update-ref",
            "refs/toimija/snapshots/old-lease",
            &legacy_tree,
        ],
    );
    write(
        root,
        ".toimija/snapshots/old-lease.json",
        &format!(
            "{{\n  \"id\": \"old-lease\",\n  \"tree\": \"{legacy_tree}\",\n  \"token\": \
             \"sha256:0000\",\n  \"pid\": 1,\n  \"created_at\": 1\n}}\n"
        ),
    );

    let fresh = json(root, &["snapshot", "--lease", "--json"]);
    assert_eq!(fresh["tree_algorithm"], TreeIdentityAlgorithm::V1.id());

    let leases = json(root, &["snapshot", "--list", "--json"]);
    let leases = leases.as_array().unwrap();
    let old = leases
        .iter()
        .find(|lease| lease["id"] == "old-lease")
        .unwrap();
    assert_eq!(
        old["tree"],
        legacy_tree.as_str(),
        "the old tree is kept as written"
    );
    assert_eq!(old["tree_algorithm"], "toimija-legacy");
    let new = leases
        .iter()
        .find(|lease| lease["id"] == fresh["id"])
        .unwrap();
    assert_eq!(new["tree_algorithm"], TreeIdentityAlgorithm::V1.id());

    let listing = toimija(root)
        .args(["snapshot", "--list"])
        .assert()
        .success();
    let listing = String::from_utf8_lossy(&listing.get_output().stdout).into_owned();
    let line = listing
        .lines()
        .find(|line| line.starts_with("old-lease"))
        .unwrap();
    assert!(line.contains("toimija-legacy"), "{listing}");

    toimija(root)
        .args(["snapshot", "--release", "old-lease"])
        .assert()
        .success();
    assert!(
        git_out(
            root,
            &["rev-parse", "--verify", "refs/toimija/snapshots/old-lease"],
            None
        )
        .is_err()
    );
}

/// A `git` placed first on PATH that moves the worktree when `read-tree`
/// runs — after the token was read, before the tree is written — `moves`
/// times, and otherwise hands every call to the real git unchanged.
struct MovingGit {
    dir: TempDir,
    _state: TempDir,
}

impl MovingGit {
    fn new(worktree: &Path, moves: u32) -> Self {
        let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join("git"))
            .find(|candidate| candidate.is_file())
            .expect("git on PATH");
        let dir = tempfile::tempdir().unwrap();
        // The count lives outside the worktree, so keeping it moves nothing.
        let state = tempfile::tempdir().unwrap();
        let counter = state.path().join("moves");
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = read-tree ]; then\n\
             \x20 n=$(cat '{counter}' 2>/dev/null || echo 0)\n\
             \x20 if [ \"$n\" -lt {moves} ]; then\n\
             \x20   n=$((n + 1))\n\
             \x20   echo \"$n\" > '{counter}'\n\
             \x20   echo \"moved $n\" > '{worktree}/moved-'\"$n\"'.txt'\n\
             \x20 fi\n\
             fi\n\
             exec '{real}' \"$@\"\n",
            counter = counter.display(),
            worktree = worktree.display(),
            real = real.display(),
        );
        let shim = dir.path().join("git");
        fs::write(&shim, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, _state: state }
    }

    fn path(&self) -> OsString {
        let mut dirs: Vec<PathBuf> = vec![self.dir.path().to_path_buf()];
        dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        std::env::join_paths(dirs).unwrap()
    }
}

/// The worktree moves once while the point's tree is written. The pair that
/// comes out is the tree and the token of one state — the settled one — and
/// never the token read before the move beside the tree written after it.
#[test]
fn repository_point_retries_when_the_worktree_moves_under_the_tree_write() {
    if !git_available() {
        return;
    }
    let temp = fixture(IGNORE_RULES[1]);
    let root = temp.path();
    let git = MovingGit::new(root, 1);

    let assert = toimija(root)
        .env("PATH", git.path())
        .args(["repository-point", "--json"])
        .assert()
        .success();
    let point: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert!(root.join("moved-1.txt").exists(), "the worktree did move");

    // The worktree is quiet now; a snapshot reads both halves of it.
    let settled = snapshot(root);
    assert_eq!(point["repository"]["tree_oid"], settled["tree"]);
    assert_eq!(
        point["repository"]["consistency_token"], settled["token"],
        "the point's token is from a different moment than its tree"
    );
}

/// A worktree that never holds still is refused, not paired.
#[test]
fn repository_point_refuses_rather_than_pair_a_tree_with_another_moments_token() {
    if !git_available() {
        return;
    }
    let temp = fixture(IGNORE_RULES[1]);
    let root = temp.path();
    let git = MovingGit::new(root, 1000);

    let assert = toimija(root)
        .env("PATH", git.path())
        .args(["repository-point", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "changed while the tree was being written",
        ));
    assert!(
        assert.get_output().stdout.is_empty(),
        "a refused point printed something"
    );
}

/// The same pairing inside a session: the sealed packet's execution context
/// holds a tree and a token of one state even when the worktree moves while
/// the entry packet is being built.
#[test]
fn a_session_packet_never_pairs_a_tree_with_another_moments_token() {
    if !git_available() {
        return;
    }
    let temp = fixture(IGNORE_RULES[1]);
    let root = temp.path();
    let git = MovingGit::new(root, 2);
    let out = tempfile::tempdir().unwrap();

    toimija(root)
        .env("PATH", git.path())
        .env("OUT", out.path())
        .args([
            "run",
            "--intent",
            "test",
            "--",
            "sh",
            "-c",
            "cp \"$TOIMIJA_LIVE_JSON\" \"$OUT/packet.json\"",
        ])
        .assert()
        .success();
    let packet: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(out.path().join("packet.json")).unwrap()).unwrap();
    assert!(root.join("moved-2.txt").exists(), "the worktree did move");

    let repository = &packet["session"]["execution"]["repository"];
    let settled = snapshot(root);
    assert_eq!(repository["tree_oid"], settled["tree"]);
    assert_eq!(repository["consistency_token"], settled["token"]);
}

/// Every git process one toimija call starts, in order, as git's own
/// `GIT_TRACE2_EVENT` records them: each argv without the program.
fn git_processes(root: &Path, args: &[&str]) -> Vec<Vec<String>> {
    let trace_dir = tempfile::tempdir().unwrap();
    let trace = trace_dir.path().join("trace2.json");
    toimija(root)
        .env("GIT_TRACE2_EVENT", &trace)
        .args(args)
        .assert()
        .success();
    fs::read_to_string(&trace)
        .unwrap()
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

/// The git subcommand of an argv, past global options such as
/// `--no-optional-locks`.
fn verb(argv: &[String]) -> &str {
    argv.iter()
        .map(String::as_str)
        .find(|arg| !arg.starts_with('-'))
        .unwrap_or("")
}

/// The budget: once the tree is written, the token is read again with one git
/// process — `status`, whose `# branch.oid` and `# branch.head` lines carry HEAD
/// and the branch — and nothing else.
#[test]
fn the_token_is_read_again_after_the_tree_write_with_one_git_process() {
    if !git_available() {
        return;
    }
    for args in [&["repository-point", "--json"][..], &["snapshot", "--json"]] {
        let temp = fixture(IGNORE_RULES[1]);
        let calls = git_processes(temp.path(), args);
        let verbs: Vec<&str> = calls.iter().map(|argv| verb(argv)).collect();
        let write = verbs
            .iter()
            .rposition(|verb| *verb == "write-tree")
            .unwrap_or_else(|| panic!("{args:?} wrote no tree: {verbs:?}"));

        // Positive controls: the trace sees Toimija's own git calls — the
        // whole v1 plan, in order, ending in the write...
        assert_eq!(
            verbs[write - 4..=write],
            ["read-tree", "add", "rm", "add", "write-tree"],
            "{args:?}: {verbs:?}"
        );
        // ...and the token's first read before it, so a re-read made of
        // `status` and more would be seen here too.
        assert!(
            verbs[..write - 4].contains(&"status"),
            "{args:?}: {verbs:?}"
        );

        let after: Vec<&str> = verbs[write + 1..].to_vec();
        assert_eq!(
            after,
            ["status"],
            "{args:?}: after the tree write: {calls:?}"
        );
    }

    // The control that matters most: the reads a four-process re-read made,
    // HEAD and the branch, are visible to this trace. repository-point still
    // makes them, once, while compiling its packet before the tree.
    let temp = fixture(IGNORE_RULES[1]);
    let calls = git_processes(temp.path(), &["repository-point", "--json"]);
    let write = calls
        .iter()
        .rposition(|argv| verb(argv) == "write-tree")
        .unwrap();
    let head = |argv: &&Vec<String>| argv.as_slice() == ["rev-parse", "HEAD"];
    let branch = |argv: &&Vec<String>| verb(argv) == "symbolic-ref";
    assert_eq!(calls[..write].iter().filter(head).count(), 1, "{calls:?}");
    assert!(calls[..write].iter().any(|argv| branch(&argv)), "{calls:?}");
    assert!(
        !calls[write..]
            .iter()
            .any(|argv| head(&argv) || branch(&argv))
    );
}
