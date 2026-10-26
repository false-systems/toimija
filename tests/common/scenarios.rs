//! Repositories in each state a packet tells apart, built the same way every
//! time. Git runs with no global or system configuration, a fixed identity and
//! fixed dates, so a scenario built twice at one path makes the same commits:
//! two builds are the same repository, down to the commit ids.

use std::fs;
use std::path::{Path, PathBuf};

use super::git_at;

/// Every scenario [`build`] knows, in the order the differential runs them.
pub const ALL: [&str; 11] = [
    "clean",
    "dirty",
    "detached",
    "linked-worktree",
    "no-upstream",
    "ahead-behind",
    "subdirectory",
    "symlinked",
    "unborn",
    "no-remote",
    "conflicted",
];

/// The environment every fixture `git`, and every toimija a test runs, shares:
/// no configuration from the machine, and a fixed identity and fixed dates.
pub const HERMETIC_GIT_ENV: [(&str, &str); 8] = [
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_AUTHOR_NAME", "Toimija Test"),
    ("GIT_AUTHOR_EMAIL", "toimija@example.invalid"),
    ("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z"),
    ("GIT_COMMITTER_NAME", "Toimija Test"),
    ("GIT_COMMITTER_EMAIL", "toimija@example.invalid"),
    ("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z"),
];

/// Run fixture `git` in `dir`, hermetically, asserting success.
pub fn git(dir: &Path, args: &[&str]) {
    let output = git_at(dir)
        .envs(HERMETIC_GIT_ENV)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run fixture `git` in `dir` where it is expected to fail (a merge that stops
/// on a conflict).
fn git_failing(dir: &Path, args: &[&str]) {
    let output = git_at(dir)
        .envs(HERMETIC_GIT_ENV)
        .args(args)
        .output()
        .unwrap();
    assert!(!output.status.success(), "git {args:?} was meant to stop");
}

pub fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// A Rust repository's first two commits: source, a manifest, the ignore rules
/// that keep Toimija's own output out of `status`, and a `repo.yaml`.
fn seed(repo: &Path) {
    write(
        repo,
        "Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    );
    write(repo, "src/main.rs", "fn main() {}\n");
    write(
        repo,
        ".gitignore",
        ".toimija/*\n!.toimija/repo.yaml\ntarget/\n",
    );
    write(
        repo,
        ".toimija/repo.yaml",
        "repo:\n  name: fixture\n  role: a repository in a known state\n  not:\n    - platform\n\
         boundaries:\n  - Stay a fixture.\n",
    );
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "init"]);
    write(repo, "README.md", "# fixture\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "readme"]);
}

/// A bare `origin` with `main`, and a clone of it at `root/repo`: what a
/// repository cloned from a forge looks like, `origin/HEAD` included.
fn cloned(root: &Path) -> PathBuf {
    let seedling = root.join("seed");
    fs::create_dir_all(&seedling).unwrap();
    git(&seedling, &["init", "-q", "-b", "main"]);
    seed(&seedling);
    git(root, &["clone", "-q", "--bare", "seed", "origin.git"]);
    fs::remove_dir_all(&seedling).unwrap();
    git(root, &["clone", "-q", "origin.git", "repo"]);
    root.join("repo")
}

/// A feature branch with one commit of its own, pushed with an upstream.
fn feature(repo: &Path) {
    git(repo, &["switch", "-q", "-c", "feat"]);
    write(repo, "src/lib.rs", "pub fn feature() {}\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "feature"]);
    git(repo, &["push", "-q", "-u", "origin", "feat"]);
}

/// Staged, unstaged, untracked and a staged rename, all at once.
fn dirty(repo: &Path) {
    write(repo, "src/new.rs", "pub fn new() {}\n");
    git(repo, &["add", "src/new.rs"]);
    git(repo, &["mv", "README.md", "README.txt"]);
    write(repo, "src/main.rs", "fn main() { println!(\"edited\"); }\n");
    write(repo, "notes.txt", "untracked\n");
}

/// Build the scenario `name` under `root`, an empty directory, and return the
/// directory toimija is to run in.
pub fn build(name: &str, root: &Path) -> PathBuf {
    match name {
        "clean" => cloned(root),
        "dirty" => {
            let repo = cloned(root);
            feature(&repo);
            dirty(&repo);
            repo
        }
        "detached" => {
            let repo = cloned(root);
            git(&repo, &["checkout", "-q", "--detach", "HEAD~1"]);
            repo
        }
        "linked-worktree" => {
            let repo = cloned(root);
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "side",
                    "../linked",
                    "origin/main",
                ],
            );
            let linked = root.join("linked");
            write(
                &linked,
                "src/main.rs",
                "fn main() { println!(\"linked\"); }\n",
            );
            write(&linked, "scratch.txt", "untracked\n");
            linked
        }
        "no-upstream" => {
            let repo = cloned(root);
            git(&repo, &["switch", "-q", "-c", "local"]);
            write(&repo, "src/local.rs", "pub fn local() {}\n");
            git(&repo, &["add", "-A"]);
            git(&repo, &["commit", "-q", "-m", "local"]);
            repo
        }
        "ahead-behind" => {
            let repo = cloned(root);
            feature(&repo);
            write(&repo, "src/pushed.rs", "pub fn pushed() {}\n");
            git(&repo, &["add", "-A"]);
            git(&repo, &["commit", "-q", "-m", "pushed"]);
            git(&repo, &["push", "-q"]);
            git(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
            write(&repo, "src/unpushed.rs", "pub fn unpushed() {}\n");
            git(&repo, &["add", "-A"]);
            git(&repo, &["commit", "-q", "-m", "unpushed"]);
            repo
        }
        "subdirectory" => {
            let repo = cloned(root);
            feature(&repo);
            dirty(&repo);
            repo.join("src")
        }
        "symlinked" => {
            let repo = cloned(root);
            feature(&repo);
            dirty(&repo);
            let link = root.join("link");
            symlink(&repo, &link);
            link
        }
        "unborn" => {
            let repo = root.join("repo");
            fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-q", "-b", "main"]);
            write(&repo, "Cargo.toml", "[package]\nname = \"fixture\"\n");
            write(&repo, "src/main.rs", "fn main() {}\n");
            repo
        }
        "no-remote" => {
            let repo = root.join("repo");
            fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-q", "-b", "main"]);
            seed(&repo);
            write(
                &repo,
                "src/main.rs",
                "fn main() { println!(\"edited\"); }\n",
            );
            repo
        }
        "conflicted" => {
            let repo = cloned(root);
            git(&repo, &["switch", "-q", "-c", "left"]);
            write(&repo, "src/main.rs", "fn main() { left(); }\n");
            git(&repo, &["commit", "-q", "-am", "left"]);
            git(&repo, &["switch", "-q", "-c", "right", "main"]);
            write(&repo, "src/main.rs", "fn main() { right(); }\n");
            git(&repo, &["commit", "-q", "-am", "right"]);
            git_failing(&repo, &["merge", "-q", "left"]);
            repo
        }
        other => panic!("no scenario named {other}"),
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(not(unix))]
fn symlink(_target: &Path, _link: &Path) {
    panic!("the symlinked scenario needs a unix filesystem");
}
