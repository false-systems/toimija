//! A repository point's id is derived from its tree.
//!
//! The id is a digest over the repository, the consistency token, the tree
//! algorithm and the tree, so two points with one id hold one tree by
//! construction. The token stays what it can honestly be: a freshness check
//! over what `git status` lists. Each known gap is a change the token does
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

/// One row: the change moved the tree, the token did not see it, `dirty`
/// is as given — and the two points have different ids.
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
/// checkout keeps namespace content outside its cone in the tree
/// (tree-identity.v2 as specified).
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
/// the tree write. tree-identity.v2
/// leaves `.sykli/` out, so the tree holds still — and since the token
/// leaves out exactly what the tree leaves out, the token holds still too.
/// A token that saw every receipt would have the point refused after three
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

    // A snapshot has no retry: one receipt during its write must not refuse it.
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

const TOKEN_VERSION: &str = "toimija-consistency-v4";

/// Every record that carries a token or a point names its version: the point
/// envelope and the session packet's execution context in `extensions`, beside
/// the tree's algorithm; snapshots and leases beside the token. The token
/// itself stays `sha256:<hex>`, the shape the seal's envelope schema requires of
/// the anchor; its hash covers the version name.
#[test]
fn points_and_tokens_name_their_version() {
    if !git_available() {
        return;
    }
    let temp = repo();
    let root = temp.path();
    write(root, "a", "a\n");
    commit(root, "init");

    let envelope = json(root, &["repository-point", "--json"]);
    assert_eq!(
        envelope["extensions"]["consistency_algorithm"],
        TOKEN_VERSION
    );
    assert_eq!(envelope["extensions"]["tree_algorithm"], "tree-identity.v2");
    let id = envelope["repository"]["id"].as_str().unwrap();
    assert!(id.starts_with("rpoint_v3_"), "{id}");
    let token = envelope["repository"]["consistency_token"]
        .as_str()
        .unwrap();
    assert!(token.starts_with("sha256:") && token.len() == 71, "{token}");

    assert_eq!(
        json(root, &["snapshot", "--json"])["algorithm"],
        TOKEN_VERSION
    );
    let lease = json(root, &["snapshot", "--lease", "--json"]);
    assert_eq!(lease["token_algorithm"], TOKEN_VERSION);

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
            "cp \"$TOIMIJA_LIVE_JSON\" \"$OUT/packet.json\" && \
             cp \"$FALSE_SYSTEMS_REPOSITORY_POINT_PATH\" \"$OUT/point.json\"",
        ])
        .assert()
        .success();
    let read = |name: &str| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(out.path().join(name)).unwrap()).unwrap()
    };
    let execution = &read("packet.json")["session"]["execution"];
    assert_eq!(
        execution["extensions"]["consistency_algorithm"],
        TOKEN_VERSION
    );
    assert_eq!(
        read("point.json")["extensions"]["consistency_algorithm"],
        TOKEN_VERSION
    );
    assert!(
        execution["repository"]["id"]
            .as_str()
            .unwrap()
            .starts_with("rpoint_v3_")
    );
}

/// A packet written by an earlier toimija carries an earlier token version.
/// Its token keeps the meaning it was written with and is never recomputed
/// under the current version — so `verify` can say neither "fresh" nor "stale" about it, only
/// "not comparable", and a refresh replaces it.
#[test]
fn a_packet_from_an_earlier_token_version_is_not_comparable() {
    if !git_available() {
        return;
    }
    let temp = repo();
    let root = temp.path();
    write(root, "a", "a\n");
    commit(root, "init");
    toimija(root).arg("refresh").assert().success();
    let path = root.join(".toimija/current.json");
    let fresh = fs::read_to_string(&path).unwrap();

    for earlier in [
        "toimija-consistency-v3",
        "toimija-consistency-v2",
        "toimija-consistency-v1",
    ] {
        let mut packet: serde_json::Value = serde_json::from_str(&fresh).unwrap();
        packet["consistency"]["algorithm"] = serde_json::json!(earlier);
        fs::write(&path, serde_json::to_string_pretty(&packet).unwrap()).unwrap();

        let assert = toimija(root).args(["verify", "--json"]).assert().failure();
        let report: serde_json::Value =
            serde_json::from_slice(&assert.get_output().stdout).unwrap();
        assert_eq!(report["status"], "not_comparable", "{report}");
        assert_eq!(report["packet_algorithm"], earlier);
        assert_eq!(report["algorithm"], TOKEN_VERSION);
        assert!(
            report["message"]
                .as_str()
                .unwrap()
                .contains("not comparable"),
            "{report}"
        );

        let assert = toimija(root).arg("verify").assert().failure();
        let text = String::from_utf8_lossy(&assert.get_output().stderr).into_owned()
            + &String::from_utf8_lossy(&assert.get_output().stdout);
        assert!(text.contains("not_comparable"), "{text}");
    }

    // An algorithm no toimija wrote is still refused as unsupported.
    let mut packet: serde_json::Value = serde_json::from_str(&fresh).unwrap();
    packet["consistency"]["algorithm"] = serde_json::json!("toimija-consistency-v9");
    fs::write(&path, serde_json::to_string_pretty(&packet).unwrap()).unwrap();
    let assert = toimija(root).args(["verify", "--json"]).assert().failure();
    let report: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(report["status"], "unsupported_consistency_algorithm");

    toimija(root).arg("refresh").assert().success();
    assert_eq!(json(root, &["verify", "--json"])["status"], "verified");
}

/// A lease taken by an earlier toimija recorded its token with no version.
/// It still lists, with its token exactly as written and no version claimed
/// for it, and still releases.
#[test]
fn a_lease_from_an_earlier_toimija_lists_its_token_unversioned_and_releases() {
    if !git_available() {
        return;
    }
    let temp = repo();
    let root = temp.path();
    write(root, ".gitignore", ".toimija/\n");
    write(root, "a", "a\n");
    commit(root, "init");
    let tree = String::from_utf8(
        git_at(root)
            .args(["rev-parse", "HEAD^{tree}"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let tree = tree.trim();
    let old_token = format!("sha256:{}", "ab".repeat(32));
    run_git(root, &["update-ref", "refs/toimija/snapshots/old", tree]);
    write(
        root,
        ".toimija/snapshots/old.json",
        &serde_json::json!({
            "id": "old", "tree": tree, "token": old_token, "pid": 1, "created_at": 1
        })
        .to_string(),
    );

    let leases = json(root, &["snapshot", "--list", "--json"]);
    let old = &leases.as_array().unwrap()[0];
    assert_eq!(old["token"], old_token.as_str(), "the token was rewritten");
    assert!(old.get("token_algorithm").is_none(), "{old}");
    assert_eq!(old["tree_algorithm"], "toimija-legacy");

    let listing = toimija(root)
        .args(["snapshot", "--list"])
        .assert()
        .success();
    let listing = String::from_utf8_lossy(&listing.get_output().stdout).into_owned();
    assert!(listing.contains("token version unrecorded"), "{listing}");

    toimija(root)
        .args(["snapshot", "--release", "old"])
        .assert()
        .success();
    assert!(!root.join(".toimija/snapshots/old.json").exists());
}

/// The id is exactly the documented digest (docs/reference.md, "Repository points"),
/// recomputed here from the envelope's own fields: `rpoint_v3_` and the
/// SHA-256 of the marker, the repository, the token's version, the token, the
/// tree's algorithm and the tree, each ended by a NUL. A reader holding an
/// envelope can check its id without toimija.
#[test]
fn the_point_id_is_the_documented_digest_of_the_envelope() {
    use sha2::{Digest, Sha256};
    if !git_available() {
        return;
    }
    let temp = repo();
    let root = temp.path();
    write(root, "a", "a\n");
    commit(root, "init");
    let envelope = json(root, &["repository-point", "--json"]);
    let point = &envelope["repository"];
    let field = |value: &serde_json::Value| value.as_str().unwrap().to_string();

    let mut digest = Sha256::new();
    for part in [
        "rpoint_v3_".to_string(),
        field(&point["repository"]),
        field(&envelope["extensions"]["consistency_algorithm"]),
        field(&point["consistency_token"]),
        field(&envelope["extensions"]["tree_algorithm"]),
        field(&point["tree_oid"]),
    ] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    assert_eq!(
        field(&point["id"]),
        format!("rpoint_v3_{:x}", digest.finalize())
    );
}
