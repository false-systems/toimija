//! Names the commit a binary was built from, for `toimija --version`:
//! `toimija 0.1.1 (a1b2c3d)`, or `(a1b2c3d, dirty)` when the code that went
//! into the binary differs from that commit. Two builds of one version number
//! are then told apart. Read with git when the build runs; a build with no git
//! or no repository (a source archive) says `unknown commit`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What the binary is built from. A difference from HEAD here makes the
/// build dirty, and a change here runs this script again.
const CODE: [&str; 5] = ["src", "build.rs", "Cargo.toml", "Cargo.lock", "vendor"];

fn main() {
    let commit = git(&["rev-parse", "--short", "HEAD"]);
    let mut status = vec!["status", "--porcelain", "--"];
    status.extend(CODE);
    let dirty = git(&status).map(|changes| !changes.is_empty());
    let build = match (commit, dirty) {
        (Some(commit), Some(true)) => format!("{commit}, dirty"),
        (Some(commit), _) => commit,
        (None, _) => "unknown commit".to_string(),
    };
    println!("cargo:rustc-env=TOIMIJA_BUILD_COMMIT={build}");

    for path in CODE {
        println!("cargo:rerun-if-changed={path}");
    }
    // And when HEAD moves: HEAD itself, the branch it names, and where refs
    // are kept when the branch's own file is not (packed-refs, a reftable).
    // Only paths that exist: cargo reruns a script whose path is missing on
    // every build.
    let Some(located) = git(&[
        "rev-parse",
        "--git-dir",
        "--git-common-dir",
        "--symbolic-full-name",
        "HEAD",
    ]) else {
        return;
    };
    let mut lines = located.lines();
    let (Some(git_dir), Some(common_dir)) = (lines.next(), lines.next()) else {
        return;
    };
    let (git_dir, common_dir) = (PathBuf::from(git_dir), PathBuf::from(common_dir));
    let mut watched = vec![git_dir.join("HEAD")];
    if let Some(branch) = lines.next().filter(|name| name.starts_with("refs/")) {
        watched.push(common_dir.join(branch));
    }
    watched.push(common_dir.join("packed-refs"));
    watched.push(common_dir.join("reftable"));
    for path in watched.iter().filter(|path| path.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// `git <args>` in this package's directory, its trimmed output when it
/// succeeds. The variables that would point git at another repository (a
/// hook's `GIT_DIR`, `GIT_INDEX_FILE`) are removed, so the package's own
/// directory decides.
fn git(args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    let package = std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_else(|| ".".into());
    command.args(args).current_dir(Path::new(&package));
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
