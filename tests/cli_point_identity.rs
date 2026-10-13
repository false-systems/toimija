//! A repository point's id is derived from its tree (KATANA D9, issue #239).
//!
//! The id is a digest over the repository, the consistency token, the tree
//! algorithm and the tree, so two points with one id hold one tree by
//! construction. The token stays what it can honestly be: a freshness check
//! over what `git status` lists. Each case in #239 is a change the token does
//! not see; for every one, the two points must have different ids. Beside each
//! row stand its controls: the change did move the tree, the token did not see
//! it (so the row exercises the blind spot it names), and an unchanged
//! repository gives the same id twice (so "different" is not noise).

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use assert_cmd::Command;
use tempfile::TempDir;

mod common;
use common::{git_at, git_available, run_git};

#[derive(Debug, Clone)]
struct Point {
    id: String,
    token: String,
    tree: String,
    dirty: bool,
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

fn point(root: &Path) -> Point {
    let envelope = json(root, &["repository-point", "--json"]);
    let point = &envelope["repository"];
    Point {
        id: point["id"].as_str().unwrap().to_string(),
        token: point["consistency_token"].as_str().unwrap().to_string(),
        tree: point["tree_oid"].as_str().unwrap().to_string(),
        dirty: point["dirty"].as_bool().unwrap(),
    }
}

fn repo() -> TempDir {
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    temp
}

/// A repository that ignores only what its own `.gitignore` says. A machine's
/// global ignore file (`core.excludesFile`, `~/.config/git/ignore`) often
/// hides `.sykli/`; these fixtures must not depend on whose machine runs them.
fn init(root: &Path) {
    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "toimija@example.invalid"]);
    run_git(root, &["config", "user.name", "Toimija Test"]);
    run_git(root, &["config", "core.excludesFile", "/dev/null"]);
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn commit(root: &Path, message: &str) {
    run_git(root, &["add", "-A"]);
    run_git(root, &["commit", "-q", "--allow-empty", "-m", message]);
}

/// Take a point, take it again unchanged (same id: the control that ids are
/// a function of the state), apply `change`, and take it once more.
fn around(root: &Path, change: impl FnOnce()) -> (Point, Point) {
    let before = point(root);
    let again = point(root);
    assert_eq!(before.id, again.id, "an unchanged repository moved its id");
    change();
    (before, point(root))
}

/// One #239 row: the change moved the tree, the token did not see it, `dirty`
/// is what the issue reported — and the two points have different ids.
fn assert_row(row: &str, (before, after): (Point, Point), dirty: bool) {
    assert_ne!(
        before.tree, after.tree,
        "row {row}: control: the change must move the tree"
    );
    assert_eq!(
        before.token, after.token,
        "row {row}: control: this is a change the token does not observe"
    );
    assert_eq!((before.dirty, after.dirty), (dirty, dirty), "row {row}");
    assert_ne!(
        before.id, after.id,
        "row {row}: one point id named two trees ({} and {})",
        before.tree, after.tree
    );
}

/// Row 1: an ignored, untracked `.toimija/repo.yaml` is edited. The tree
/// forces it in; `git status` never lists it.
#[test]
fn row_1_an_ignored_repo_yaml_edit_gets_a_new_point() {
    if !git_available() {
        return;
    }
    let temp = repo();
    let root = temp.path();
    write(root, ".gitignore", ".toimija/\n");
    write(root, "a", "a\n");
    commit(root, "init");
    write(root, ".toimija/repo.yaml", "repo: {name: one}\n");

    let row = around(root, || {
        write(root, ".toimija/repo.yaml", "repo: {name: two}\n")
    });
    assert_row("1", row, false);
}

/// Row 1b: that file is created.
#[test]
fn row_1b_an_ignored_repo_yaml_appearing_gets_a_new_point() {
    if !git_available() {
        return;
    }
    let temp = repo();
    let root = temp.path();
    write(root, ".gitignore", ".toimija/\n");
    write(root, "a", "a\n");
    commit(root, "init");

    let row = around(root, || {
        write(root, ".toimija/repo.yaml", "repo: {name: one}\n")
    });
    assert_row("1b", row, false);
}

/// Row 2: a second commit inside a submodule that is already ahead. The
/// token sees the gitlink as a directory, never its commit.
#[test]
fn row_2_a_commit_in_a_submodule_already_ahead_gets_a_new_point() {
    if !git_available() {
        return;
    }
    let upstream = repo();
    write(upstream.path(), "lib.rs", "lib\n");
    commit(upstream.path(), "lib");

    let temp = repo();
    let root = temp.path();
    write(root, "a", "a\n");
    commit(root, "init");
    let source = upstream.path().display().to_string();
    run_git(
        root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &source,
            "sub",
        ],
    );
    commit(root, "add sub");
    let sub = root.join("sub");
    let sub_commit = |line: &str| {
        let lib = sub.join("lib.rs");
        let mut contents = fs::read_to_string(&lib).unwrap();
        contents.push_str(line);
        fs::write(&lib, contents).unwrap();
        run_git(
            &sub,
            &[
                "-c",
                "user.email=toimija@example.invalid",
                "-c",
                "user.name=Toimija Test",
                "commit",
                "-q",
                "-am",
                line,
            ],
        );
    };
    sub_commit("x\n");

    let row = around(root, || sub_commit("y\n"));
    assert_row("2", row, true);
}

/// Row 3: a commit inside a nested, untracked repository. The tree records it
/// as a gitlink at its HEAD; the token sees an untracked directory.
#[test]
fn row_3_a_commit_in_a_nested_repository_gets_a_new_point() {
    if !git_available() {
        return;
    }
    let temp = repo();
    let root = temp.path();
    write(root, "a", "a\n");
    commit(root, "init");
    let nested = root.join("nested");
    fs::create_dir_all(&nested).unwrap();
    init(&nested);
    write(&nested, "n", "1\n");
    commit(&nested, "1");

    let row = around(root, || {
        write(&nested, "n", "2\n");
        commit(&nested, "2");
    });
    assert_row("3", row, true);
}

/// Rows 4 and 5: a file whose index entry carries `flag` is edited. `git
/// status` trusts the flag; the tree is written from a scratch index that has
/// none.
fn flagged_edit(row: &str, flag: &str) {
    let temp = repo();
    let root = temp.path();
    write(root, "a", "a\n");
    write(root, "b", "b\n");
    commit(root, "init");
    run_git(root, &["update-index", flag, "a"]);

    let pair = around(root, || write(root, "a", "edited\n"));
    assert_row(row, pair, false);
}

#[test]
fn row_4_an_assume_unchanged_edit_gets_a_new_point() {
    if !git_available() {
        return;
    }
    flagged_edit("4", "--assume-unchanged");
}

#[test]
fn row_5_a_skip_worktree_edit_gets_a_new_point() {
    if !git_available() {
        return;
    }
    flagged_edit("5", "--skip-worktree");
}

/// Row 6: `core.checkStat=minimal` with `core.trustctime=false`, and a
/// same-size edit with the mtime put back. The stat cache misses it.
#[test]
fn row_6_an_edit_the_stat_cache_misses_gets_a_new_point() {
    if !git_available() {
        return;
    }
    let temp = repo();
    let root = temp.path();
    run_git(root, &["config", "core.checkStat", "minimal"]);
    run_git(root, &["config", "core.trustctime", "false"]);
    write(root, "a", "aaaa");
    commit(root, "init");
    let file = root.join("a");
    // 2026-01-01T00:00:00Z: well before the index is written, so not racy.
    let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600);
    let restore_mtime = || {
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    };
    restore_mtime();
    // Let the index learn that mtime; "needs update" is not a failure here.
    let _ = git_at(root)
        .args(["update-index", "-q", "--refresh"])
        .output()
        .unwrap();

    let row = around(root, || {
        fs::write(&file, "bbbb").unwrap();
        restore_mtime();
    });
    assert_row("6", row, false);
}

/// Row 7: one commit, in a full checkout and then a sparse one. The sparse
/// checkout keeps namespace content outside its cone in the tree (faija's
/// tree-identity.v1 as specified; issue N3a in the #238 review).
#[test]
fn row_7_a_sparse_and_a_full_checkout_of_one_commit_get_different_points() {
    if !git_available() {
        return;
    }
    let temp = repo();
    let root = temp.path();
    write(root, "src/a.rs", "a\n");
    write(root, ".sykli/tracked.json", "{}\n");
    commit(root, "init");

    let row = around(root, || {
        run_git(root, &["sparse-checkout", "set", "--cone", "src"]);
        assert!(!root.join(".sykli").exists(), "the cone did not apply");
    });
    assert_row("7", row, false);
}

/// A `git` placed first on PATH that runs `pre_add` before the first
/// `times` calls of `git add -A`, and `post_write` after the first
/// `write-tree` (keeping its output and status); every call otherwise goes to
/// the real git unchanged. Its counters, and anything the hooks keep in
/// `$STATE`, live outside the worktree.
struct Shim {
    dir: TempDir,
    state: TempDir,
}

impl Shim {
    fn new(worktree: &Path, pre_add: &str, times: u32, post_write: &str) -> Self {
        let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join("git"))
            .find(|candidate| candidate.is_file())
            .expect("git on PATH");
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let script = format!(
            "#!/bin/sh\n\
             STATE='{state}'\n\
             fire() {{\n\
             \x20 n=$(cat \"$STATE/$1\" 2>/dev/null || echo 0)\n\
             \x20 [ \"$n\" -lt \"$2\" ] || return 1\n\
             \x20 echo $((n + 1)) > \"$STATE/$1\"\n\
             }}\n\
             if [ \"$1\" = add ] && [ \"$2\" = -A ] && fire add {times}; then\n\
             \x20 (cd '{worktree}' && {pre_add})\n\
             fi\n\
             if [ \"$1\" = write-tree ] && fire write 1; then\n\
             \x20 '{real}' \"$@\"; rc=$?\n\
             \x20 (cd '{worktree}' && {post_write})\n\
             \x20 exit $rc\n\
             fi\n\
             exec '{real}' \"$@\"\n",
            state = state.path().display(),
            worktree = worktree.display(),
            real = real.display(),
        );
        let shim = dir.path().join("git");
        fs::write(&shim, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, state }
    }

    fn path(&self) -> OsString {
        let mut dirs: Vec<PathBuf> = vec![self.dir.path().to_path_buf()];
        dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        std::env::join_paths(dirs).unwrap()
    }

    fn fired(&self, name: &str) -> u32 {
        fs::read_to_string(self.state.path().join(name))
            .map(|count| count.trim().parse().unwrap())
            .unwrap_or(0)
    }
}

/// A → B → A while the tree is written: the token, read before and after,
/// sees A both times, and the tree holds B. The point still names its tree
/// honestly — its id is not the settled point's, though the tokens are equal.
/// Identity is closed by construction; freshness is not: the token cannot
/// tell this point from the settled one.
#[test]
fn a_change_undone_during_the_write_gets_its_own_point() {
    if !git_available() {
        return;
    }
    let temp = repo();
    let root = temp.path();
    write(root, ".gitignore", ".toimija/\n");
    write(root, "src/a.rs", "a\n");
    commit(root, "init");
    let shim = Shim::new(
        root,
        "cp src/a.rs \"$STATE/a.bak\" && echo B >> src/a.rs",
        1,
        "cp \"$STATE/a.bak\" src/a.rs",
    );

    let assert = toimija(root)
        .env("PATH", shim.path())
        .args(["repository-point", "--json"])
        .assert()
        .success();
    let envelope: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!((shim.fired("add"), shim.fired("write")), (1, 1));
    let moved = &envelope["repository"];
    let settled = point(root);

    assert_ne!(
        moved["tree_oid"], settled.tree,
        "control: the write saw B, not A"
    );
    assert_eq!(
        moved["consistency_token"], settled.token,
        "control: the token cannot see A → B → A"
    );
    assert_ne!(
        moved["id"], settled.id,
        "one point id named the tree of B and the tree of A"
    );
}

/// sykli writes receipts into a `.sykli/` that git can see, at every step of
/// the tree write (the #238 review's `sykli-churn.sh`). tree-identity.v1
/// leaves `.sykli/` out, so the tree holds still — and since the token now
/// leaves out exactly what the tree leaves out, the token holds still too.
/// Before, the token saw every receipt, and the point was refused after three
/// attempts: "the tree and the token would describe different states".
#[test]
fn sykli_writing_receipts_never_makes_a_point_refuse() {
    if !git_available() {
        return;
    }
    let temp = repo();
    let root = temp.path();
    write(root, "a", "a\n");
    commit(root, "init");
    write(root, ".sykli/receipt.json", "{\"outcome\":\"passed\"}\n");
    let quiet = point(root);
    let shim = Shim::new(root, "echo r > .sykli/receipt-$n.json", 99, ":");

    let assert = toimija(root)
        .env("PATH", shim.path())
        .args(["repository-point", "--json"])
        .assert()
        .success();
    let envelope: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert!(shim.fired("add") >= 1, "control: sykli wrote nothing");
    assert!(root.join(".sykli/receipt-0.json").exists());
    let churned = &envelope["repository"];
    assert_eq!(churned["tree_oid"], quiet.tree, "a receipt moved the tree");
    assert_eq!(
        churned["consistency_token"], quiet.token,
        "a receipt moved the token"
    );
    assert_eq!(churned["id"], quiet.id);

    // A snapshot has no retry: one receipt during its write used to refuse it.
    let shim = Shim::new(root, "echo r > .sykli/snap-$n.json", 99, ":");
    let assert = toimija(root)
        .env("PATH", shim.path())
        .args(["snapshot", "--json"])
        .assert()
        .success();
    let snapshot: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert!(shim.fired("add") >= 1, "control: sykli wrote nothing");
    assert_eq!(snapshot["tree"], quiet.tree);
    assert_eq!(snapshot["token"], quiet.token);
}

/// The gate a repository declares writes `.sykli/receipt.json`, and then the
/// agent runs `toimija verify` before committing. A receipt is generated
/// state, so the packet stays fresh; work content does not.
#[test]
fn a_sykli_receipt_does_not_make_the_packet_stale() {
    if !git_available() {
        return;
    }
    let temp = repo();
    let root = temp.path();
    write(root, "a", "a\n");
    commit(root, "init");
    toimija(root).arg("refresh").assert().success();

    write(root, ".sykli/receipt.json", "{\"outcome\":\"passed\"}\n");
    write(root, ".sykli/runs/r1/journal", "started\n");
    let verified = json(root, &["verify", "--json"]);
    assert_eq!(verified["status"], "verified", "{verified}");

    // Control: the same check does see work content.
    write(root, "b", "b\n");
    let assert = toimija(root).args(["verify", "--json"]).assert().failure();
    let stale: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(stale["status"], "stale_packet", "{stale}");
}
