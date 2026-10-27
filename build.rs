//! Names the commit a binary was built from, for `toimija --version`:
//! `toimija 0.1.1 (a1b2c3d)`, or `(a1b2c3d, dirty)` when the code that went
//! into the binary differs from that commit. Two builds of one version number
//! are then told apart. Read with git when the build runs. A build from a tree
//! that is not its own repository's root (a source archive, unpacked inside
//! some other repository or none) says `unknown commit`, never another
//! repository's commit.
//!
//! `tests/cli_help.rs` includes this file to test [`describe`].

use std::path::{Path, PathBuf};
use std::process::Command;

/// What the binary is built from. A difference from HEAD here makes the
/// build dirty, and a change here runs this script again.
const CODE: [&str; 5] = ["src", "build.rs", "Cargo.toml", "Cargo.lock", "vendor"];

/// What [`describe`] found: the commit text for `--version`, and the git
/// files whose change means HEAD moved.
#[derive(Debug)]
pub struct Build {
    pub commit: String,
    pub watched: Vec<PathBuf>,
}

#[allow(dead_code)] // `tests/cli_help.rs` includes this file without running it.
fn main() {
    let package = std::env::var_os("CARGO_MANIFEST_DIR").map_or_else(|| ".".into(), PathBuf::from);
    let build = describe(&package);
    println!("cargo:rustc-env=TOIMIJA_BUILD_COMMIT={}", build.commit);
    for path in CODE {
        println!("cargo:rerun-if-changed={path}");
    }
    for path in &build.watched {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// The commit `package` is built from, when `package` is the root of its own
/// repository, and the git files to watch for HEAD moving.
pub fn describe(package: &Path) -> Build {
    let unknown = || Build {
        commit: "unknown commit".to_string(),
        watched: Vec::new(),
    };
    let Some(located) = git(
        package,
        &[
            "rev-parse",
            "--show-toplevel",
            "--git-dir",
            "--git-common-dir",
            "--symbolic-full-name",
            "HEAD",
        ],
    ) else {
        return unknown();
    };
    let mut lines = located.lines();
    let (Some(toplevel), Some(git_dir), Some(common_dir)) =
        (lines.next(), lines.next(), lines.next())
    else {
        return unknown();
    };
    // The repository git found must be the package's own: a source tree
    // unpacked inside another repository is not that repository's commit.
    let own = |path: &Path| std::fs::canonicalize(path).ok();
    if own(Path::new(toplevel)).is_none() || own(Path::new(toplevel)) != own(package) {
        return unknown();
    }
    let Some(commit) = git(package, &["rev-parse", "--short", "HEAD"]) else {
        return unknown();
    };
    let mut status = vec!["status", "--porcelain", "--"];
    status.extend(CODE);
    let commit = match git(package, &status) {
        Some(changes) if !changes.is_empty() => format!("{commit}, dirty"),
        _ => commit,
    };

    // HEAD moves: HEAD itself, the branch it names, where refs are kept when
    // the branch's own file is not (packed-refs, a reftable), and HEAD's
    // reflog, which every commit, checkout and reset appends to, whichever of
    // those holds the branch (a commit after `pack-refs` writes a loose ref
    // this script did not see at its last run). Only paths that exist: cargo
    // reruns a script whose path is missing on every build.
    let at = |dir: &str| {
        let dir = Path::new(dir);
        if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            package.join(dir)
        }
    };
    let (git_dir, common_dir) = (at(git_dir), at(common_dir));
    let mut watched = vec![git_dir.join("HEAD"), git_dir.join("logs/HEAD")];
    if let Some(branch) = lines.next().filter(|name| name.starts_with("refs/")) {
        watched.push(common_dir.join(branch));
    }
    watched.push(common_dir.join("packed-refs"));
    watched.push(common_dir.join("reftable"));
    watched.retain(|path| path.exists());
    Build { commit, watched }
}

/// `git <args>` in `package`, its trimmed output when it succeeds. The
/// variables that would point git at another repository (a hook's `GIT_DIR`,
/// `GIT_INDEX_FILE`) are removed, so the package's directory decides.
fn git(package: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command.args(args).current_dir(package);
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        command.env_remove(key);
    }
    let output = command.output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}
