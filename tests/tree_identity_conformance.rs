//! Conformance tests for `tree-identity.v1`.
//!
//! These tests run the plan with real `git`, step by step and without
//! Toimija's commands, so they prove properties of the rule itself: what moves
//! the identity and what does not.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use tempfile::TempDir;
use toimija::tree_identity::{Step, TreeIdentityAlgorithm};

mod common;
use common::{git_at, git_available};

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            dir: tempfile::tempdir().expect("create fixture"),
        };
        fixture.git(&["init", "--quiet"]);
        fixture.git(&["config", "user.email", "fixture@localhost"]);
        fixture.git(&["config", "user.name", "Fixture"]);
        fixture
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, contents).expect("write fixture file");
    }

    fn git(&self, args: &[&str]) -> String {
        self.git_with_index(None, args)
            .unwrap_or_else(|error| panic!("git {args:?} failed: {error}"))
    }

    fn git_with_index(&self, index: Option<&Path>, args: &[&str]) -> Result<String, String> {
        let mut command = git_at(self.root());
        command.args(args);
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        let output = command.output().map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn scratch_index(&self, tag: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let index = self
            .root()
            .join(format!(".git/tree-identity-{tag}-{unique}.index"));
        let _ = std::fs::remove_file(&index);
        index
    }

    /// Paths present in a tree, so a test can assert membership instead of
    /// comparing OIDs across fixtures whose unrelated content differs.
    fn tree_paths(&self, tree: &str) -> Vec<String> {
        self.git(&["ls-tree", "-r", "--name-only", tree])
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "--quiet", "-m", message]);
    }

    /// Execute the canonical plan exactly as it is returned.
    fn canonical_tree(&self) -> String {
        let index = self.scratch_index("canonical");
        for step in TreeIdentityAlgorithm::V1.plan() {
            match step {
                Step::SeedFromHead => {
                    // Tolerated: an unborn HEAD has no tracked files.
                    let _ = self.git_with_index(Some(&index), &["read-tree", "HEAD"]);
                }
                Step::OverlayWorktree { excludes } => {
                    let mut args = vec!["add".to_string(), "-A".to_string(), "--".to_string()];
                    args.extend(excludes);
                    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
                    self.git_with_index(Some(&index), &borrowed)
                        .expect("overlay worktree");
                }
                Step::DropNamespaces { paths } => {
                    let mut args = vec![
                        "rm".to_string(),
                        "--cached".to_string(),
                        "-r".to_string(),
                        "-q".to_string(),
                        "--ignore-unmatch".to_string(),
                    ];
                    args.extend(paths);
                    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
                    self.git_with_index(Some(&index), &borrowed)
                        .expect("drop namespaces");
                }
                Step::ForceAddConfig { path } => {
                    if self.root().join(&path).is_file() {
                        self.git_with_index(Some(&index), &["add", "-f", "--", &path])
                            .expect("force-add config");
                    }
                }
                Step::WriteTree => {}
            }
        }
        let tree = self
            .git_with_index(Some(&index), &["write-tree"])
            .expect("write-tree");
        let _ = std::fs::remove_file(&index);
        tree
    }
}

/// Under the canonical plan, configuration is in the tree whatever the
/// repository's ignore rules say. Asserted as membership, because the two
/// fixtures necessarily differ in `.gitignore`, which is itself work content.
#[test]
fn t4_canonical_identity_includes_config_whatever_the_ignore_status() {
    if !git_available() {
        return;
    }
    let ignored = Fixture::new();
    ignored.write("src/main.rs", "fn main() {}\n");
    ignored.write(".gitignore", ".toimija/\n.sykli/\n.teko/\n");
    ignored.write(".toimija/repo.yaml", "repo:\n  name: fixture\n");
    ignored.commit("initial");

    let tracked = Fixture::new();
    tracked.write("src/main.rs", "fn main() {}\n");
    tracked.write(
        ".gitignore",
        ".toimija/*\n!.toimija/repo.yaml\n.sykli/\n.teko/\n",
    );
    tracked.write(".toimija/repo.yaml", "repo:\n  name: fixture\n");
    tracked.commit("initial");

    for fixture in [&ignored, &tracked] {
        let tree = fixture.canonical_tree();
        let paths = fixture.tree_paths(&tree);
        assert!(
            paths.contains(&".toimija/repo.yaml".to_string()),
            "configuration missing from the canonical tree: {paths:?}"
        );
        assert!(
            !paths.iter().any(|path| path.starts_with(".sykli/")),
            "generated state present in the canonical tree: {paths:?}"
        );
    }
}

/// The same repository state gives the same identity, run twice.
#[test]
fn t1_the_plan_is_reproducible_from_its_own_description() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write("src/main.rs", "fn main() {}\n");
    fixture.write(".gitignore", ".toimija/\n.sykli/\n");
    fixture.write(".toimija/repo.yaml", "repo:\n  name: fixture\n");
    fixture.write(".sykli/receipt.json", "{}\n");
    fixture.commit("initial");

    assert_eq!(
        fixture.canonical_tree(),
        fixture.canonical_tree(),
        "the plan is not deterministic"
    );
}

/// Generated state in any of the frozen namespaces must not move the identity.
#[test]
fn t2_generated_state_in_any_namespace_does_not_change_identity() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write("src/lib.rs", "pub fn f() {}\n");
    fixture.write(".toimija/repo.yaml", "repo:\n  name: fixture\n");
    fixture.commit("initial");
    let before = fixture.canonical_tree();

    for namespace in TreeIdentityAlgorithm::V1.namespaces() {
        fixture.write(&format!("{namespace}/generated.json"), "{\"noise\":true}\n");
    }
    fixture.write(".toimija/sessions/abc/current.json", "{\"live\":true}\n");

    assert_eq!(
        before,
        fixture.canonical_tree(),
        "generated tool state changed the work identity"
    );
}

/// Configuration is an input to the work, so it must move the identity — and
/// must do so whether or not the repository gitignores it.
#[test]
fn t3_configuration_changes_identity_even_when_gitignored() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write("src/lib.rs", "pub fn f() {}\n");
    fixture.write(".gitignore", ".toimija/\n");
    fixture.write(".toimija/repo.yaml", "repo:\n  name: before\n");
    fixture.commit("initial");
    let before = fixture.canonical_tree();

    fixture.write(".toimija/repo.yaml", "repo:\n  name: after\n");
    assert_ne!(
        before,
        fixture.canonical_tree(),
        "a configuration change must change the work identity"
    );
}

/// Ignored files outside a namespace stay out; untracked non-ignored files come in.
#[test]
fn t5_ignore_rules_apply_outside_namespaces() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write("src/lib.rs", "pub fn f() {}\n");
    fixture.write(".gitignore", "target/\n");
    fixture.commit("initial");
    let baseline = fixture.canonical_tree();

    fixture.write("target/debug/binary", "artifact\n");
    assert_eq!(
        baseline,
        fixture.canonical_tree(),
        "an ignored build directory changed the identity"
    );

    fixture.write("src/new.rs", "pub fn g() {}\n");
    assert_ne!(
        baseline,
        fixture.canonical_tree(),
        "an untracked source file was invisible to the identity"
    );
}

/// A namespace file that is committed is still generated state.
#[test]
fn t6_a_tracked_namespace_file_is_still_excluded() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write("src/lib.rs", "pub fn f() {}\n");
    fixture.write(".toimija/repo.yaml", "repo:\n  name: fixture\n");
    fixture.commit("initial");
    let before = fixture.canonical_tree();

    // Commit generated state deliberately, the way a repository might.
    fixture.write(".sykli/receipt.json", "{\"outcome\":\"passed\"}\n");
    fixture.git(&["add", "-f", "--", ".sykli/receipt.json"]);
    fixture.commit("track a receipt");

    assert_eq!(
        before,
        fixture.canonical_tree(),
        "committing generated state changed the work identity"
    );
}

/// The excludes are pathspecs git actually accepts.
#[test]
fn excludes_are_valid_pathspecs() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write("src/lib.rs", "pub fn f() {}\n");
    fixture.commit("initial");
    let index = fixture.scratch_index("pathspec");
    let mut args = vec!["add".to_string(), "-A".to_string(), "--".to_string()];
    args.extend(TreeIdentityAlgorithm::V1.excludes());
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    fixture
        .git_with_index(Some(&index), &borrowed)
        .expect("git rejected a canonical exclude pathspec");
}

/// The danger of speculative reservation, proven against real git: content
/// under a directory nobody reserved is ordinary work content and must move the
/// identity. A directory frozen into v1 without generated state in it would
/// make this content invisible to work identity.
#[test]
fn t7_content_under_an_unreserved_namespace_contributes_to_identity() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write("src/lib.rs", "pub fn f() {}\n");
    fixture.commit("initial");
    let before = fixture.canonical_tree();

    for unreserved in [".widget", ".example", ".acme"] {
        fixture.write(&format!("{unreserved}/meaningful.md"), "real content\n");
        assert_ne!(
            before,
            fixture.canonical_tree(),
            "{unreserved} content vanished from work identity"
        );
        std::fs::remove_dir_all(fixture.root().join(unreserved)).expect("clean up");
        assert_eq!(
            before,
            fixture.canonical_tree(),
            "removing {unreserved} did not restore the identity"
        );
    }
}
