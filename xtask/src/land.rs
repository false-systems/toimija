//! `cargo xtask land`: put this branch on the remote's `main` without a pull
//! request, when it is proven and reviewed.
//!
//! It refuses unless all of these hold for the commit checked out:
//!
//! - nothing is uncommitted, and the remote's `main` is an ancestor of it, so
//!   the push is a fast-forward and the tree that lands is the tree checked;
//! - sykli says its tree passed (`sykli status --commit`), or a receipt
//!   another clone shared verifies here (`sykli verify`);
//! - a review of this tree against that `main` is kept, and it is not
//!   blocking, or a person overruled it.
//!
//! Then it pushes the commit to `main`, and the review and the receipt as git
//! notes on the tree (`refs/notes/toimija/review`, `refs/notes/sykli/receipt`),
//! so another clone can check the commit without running anything again.
//!
//! Exit codes: 0 landed, 1 refused (it says what is missing), 2 it could not
//! tell or the push failed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::review::{self, git, Level};

/// Where receipts travel: one note per tree, the receipt as sykli wrote it.
pub const RECEIPTS: &str = "refs/notes/sykli/receipt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proof {
    /// The tree passed. `receipt` is the file to share, unless the proof is
    /// itself a shared receipt.
    Passed {
        receipt: Option<PathBuf>,
    },
    Not(String),
}

/// Says whether a commit's tree is proven: `(root, commit, tree)`.
pub type Prover<'a> = &'a dyn Fn(&Path, &str, &str) -> Result<Proof>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Landed { head: String, review: Level },
    Refused(String),
}

pub fn land(root: &Path, remote: &str, prover: Prover<'_>) -> Result<Outcome> {
    if !git(root, &["status", "--porcelain"])?.is_empty() {
        return refused("uncommitted or untracked files here: commit first");
    }
    git(root, &["fetch", "--quiet", remote, "main"])?;
    fetch_notes(root, remote)?;
    let target_ref = format!("{remote}/main");
    let target = git(root, &["rev-parse", &target_ref])?;
    let head = git(root, &["rev-parse", "HEAD"])?;
    let tree = git(root, &["rev-parse", "HEAD^{tree}"])?;
    if head == target {
        return refused(&format!(
            "nothing to land: {target_ref} is already this commit"
        ));
    }
    if !is_ancestor(root, &target, &head)? {
        return refused(&format!(
            "{target_ref} has commits this branch lacks: merge it in, then prove and review \
             the result"
        ));
    }
    let receipt = match prover(root, &head, &tree)? {
        Proof::Passed { receipt } => receipt,
        Proof::Not(why) => return refused(&format!("not proven ({why}): run `sykli run`")),
    };
    let Some(record) = review::read_record(root, &tree)?.filter(|kept| kept.base == target) else {
        return refused("not reviewed against this main: run `cargo xtask review`");
    };
    if review::exit_code(&record) != 0 {
        return refused(&format!(
            "the review is blocking ({} finding(s)): fix it, or a person overrules it",
            record.findings.len()
        ));
    }
    if let Some(receipt) = receipt {
        let path = receipt.to_string_lossy();
        git(
            root,
            &["notes", "--ref", RECEIPTS, "add", "-f", "-F", &path, &tree],
        )
        .context("could not attach the receipt to the tree")?;
    }
    git(
        root,
        &[
            "push",
            "--quiet",
            remote,
            &format!("{head}:refs/heads/main"),
        ],
    )
    .context("the push to main was refused")?;
    push_notes(root, remote).with_context(|| {
        format!("{head} is on {remote}'s main, but its review and receipt were not shared")
    })?;
    Ok(Outcome::Landed {
        head,
        review: record.level,
    })
}

fn refused(why: &str) -> Result<Outcome> {
    Ok(Outcome::Refused(why.to_string()))
}

fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let status = Command::new("git")
        .current_dir(root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("git merge-base --is-ancestor {ancestor} {descendant} failed"),
    }
}

/// Brings the remote's reviews and receipts into this clone's notes. Where
/// both hold a note for a tree, this clone's stays.
pub fn fetch_notes(root: &Path, remote: &str) -> Result<()> {
    for notes in [review::NOTES, RECEIPTS] {
        if git(root, &["ls-remote", remote, notes])?.is_empty() {
            continue;
        }
        let fetched = notes.replacen("refs/notes/", "refs/notes-fetched/", 1);
        git(
            root,
            &["fetch", "--quiet", remote, &format!("+{notes}:{fetched}")],
        )?;
        if git(root, &["rev-parse", "--quiet", "--verify", notes]).is_err() {
            git(root, &["update-ref", notes, &fetched])?;
        } else {
            git(
                root,
                &[
                    "notes", "--ref", notes, "merge", "--quiet", "-s", "ours", &fetched,
                ],
            )?;
        }
    }
    Ok(())
}

fn push_notes(root: &Path, remote: &str) -> Result<()> {
    let kept: Vec<&str> = [review::NOTES, RECEIPTS]
        .into_iter()
        .filter(|notes| git(root, &["rev-parse", "--quiet", "--verify", notes]).is_ok())
        .collect();
    if kept.is_empty() {
        return Ok(());
    }
    let mut push = vec!["push", "--quiet", remote];
    push.extend(&kept);
    if git(root, &push).is_ok() {
        return Ok(());
    }
    // Another clone shared notes meanwhile: take them in and push once more.
    fetch_notes(root, remote)?;
    git(root, &push).map(|_| ())
}

/// Asks sykli: first what this clone's receipts say of the commit, then
/// whether a receipt another clone shared for its tree verifies here.
pub fn sykli_proof(root: &Path, commit: &str, tree: &str) -> Result<Proof> {
    let output = Command::new("sykli")
        .current_dir(root)
        .args(["status", "--commit", commit, "--json"])
        .output()
        .context("could not start sykli")?;
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        anyhow!(
            "sykli status --commit gave no answer: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    })?;
    let verdict = status["verdict"].as_str().unwrap_or("unknown").to_string();
    if verdict == "passed" {
        let receipt = status["current"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["outcome"] == "passed" && entry["judgement"] == "verified")
            .and_then(|entry| entry["receipt"].as_str())
            .map(PathBuf::from)
            .filter(|path| path.is_file());
        return Ok(Proof::Passed { receipt });
    }
    let shared = Command::new("git")
        .current_dir(root)
        .args(["notes", "--ref", RECEIPTS, "show", tree])
        .output()?;
    if !shared.status.success() {
        return Ok(Proof::Not(format!("sykli says {verdict}")));
    }
    let copy = std::env::temp_dir().join(format!(
        "toimija-shared-receipt-{}.json",
        std::process::id()
    ));
    fs::write(&copy, &shared.stdout)?;
    let verified = Command::new("sykli")
        .current_dir(root)
        .args(["verify"])
        .arg(&copy)
        .args(["--contract", "sykli.json", "--commit", commit])
        .output();
    // The copy is ours; a failure to remove it changes nothing.
    let _ = fs::remove_file(&copy);
    let verified = verified.context("could not start sykli")?;
    if !verified.status.success() {
        return Ok(Proof::Not(format!(
            "sykli says {verdict}, and the shared receipt does not verify"
        )));
    }
    let receipt: serde_json::Value = serde_json::from_slice(&shared.stdout)?;
    let all_passed = receipt["tasks"].as_array().is_some_and(|tasks| {
        !tasks.is_empty() && tasks.iter().all(|task| task["outcome"] == "passed")
    });
    if all_passed {
        Ok(Proof::Passed { receipt: None })
    } else {
        Ok(Proof::Not(
            "the shared receipt verifies, but not every task in it passed".to_string(),
        ))
    }
}

/// `cargo xtask land [--remote NAME]`, returning the exit code.
pub fn main(root: &Path, args: &[String]) -> u8 {
    let remote = match args {
        [] => "origin",
        [flag, remote] if flag == "--remote" => remote.as_str(),
        _ => {
            eprintln!("land: usage: cargo xtask land [--remote NAME]");
            return 2;
        }
    };
    match land(root, remote, &sykli_proof) {
        Ok(Outcome::Landed { head, review }) => {
            eprintln!(
                "land: {} is on {remote}'s main: proven, review {review}; both shared",
                &head[..12.min(head.len())]
            );
            0
        }
        Ok(Outcome::Refused(why)) => {
            eprintln!("land: refused: {why}");
            1
        }
        Err(error) => {
            eprintln!("land: {error:#}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::{overrule, Agent};
    use std::time::{SystemTime, UNIX_EPOCH};

    const PASSED: Prover<'static> = &|_, _, _| Ok(Proof::Passed { receipt: None });

    #[test]
    fn a_proven_reviewed_branch_lands_and_its_review_reaches_another_clone() {
        let Some(world) = World::new("land-ok") else {
            return;
        };
        world.review("REVIEW: look\n");
        let receipt = world.dir.join("receipt.json");
        fs::write(&receipt, "{\"tasks\":[]}\n").unwrap();
        let with_receipt = |_: &Path, _: &str, _: &str| {
            Ok(Proof::Passed {
                receipt: Some(receipt.clone()),
            })
        };

        let outcome = land(&world.work, "origin", &with_receipt).unwrap();
        let head = world.git(&["rev-parse", "HEAD"]);
        assert_eq!(
            outcome,
            Outcome::Landed {
                head: head.clone(),
                review: Level::Look
            }
        );
        assert_eq!(world.remote_main(), head);

        // Another clone has run nothing: it reads both from the remote.
        let other = world.dir.join("other");
        run(&world.dir, &["clone", "-q", "origin.git", "other"]);
        fetch_notes(&other, "origin").unwrap();
        let tree = world.git(&["rev-parse", "HEAD^{tree}"]);
        let shared = review::read_record(&other, &tree).unwrap().unwrap();
        assert_eq!(shared.level, Level::Look);
        assert_eq!(
            git(&other, &["notes", "--ref", RECEIPTS, "show", &tree]).unwrap(),
            "{\"tasks\":[]}"
        );
    }

    #[test]
    fn an_unproven_unreviewed_or_blocked_branch_does_not_land() {
        let Some(world) = World::new("land-refused") else {
            return;
        };
        let before = world.remote_main();
        let unproven = |_: &Path, _: &str, _: &str| Ok(Proof::Not("sykli says none".to_string()));

        let refusal = |outcome: Outcome| match outcome {
            Outcome::Refused(why) => why,
            Outcome::Landed { .. } => panic!("it landed"),
        };
        let why = refusal(land(&world.work, "origin", &unproven).unwrap());
        assert!(why.contains("not proven"), "{why}");
        let why = refusal(land(&world.work, "origin", PASSED).unwrap());
        assert!(why.contains("not reviewed"), "{why}");
        world.review("- [blocking] new.txt:1: wrong.\nREVIEW: blocking\n");
        let why = refusal(land(&world.work, "origin", PASSED).unwrap());
        assert!(why.contains("blocking"), "{why}");
        fs::write(world.work.join("stray.txt"), "x\n").unwrap();
        let why = refusal(land(&world.work, "origin", PASSED).unwrap());
        assert!(why.contains("commit first"), "{why}");
        fs::remove_file(world.work.join("stray.txt")).unwrap();
        assert_eq!(world.remote_main(), before, "nothing reached main");

        overrule(&world.work, "origin/main", "accepted by a person").unwrap();
        assert!(matches!(
            land(&world.work, "origin", PASSED).unwrap(),
            Outcome::Landed { .. }
        ));
    }

    #[test]
    fn a_branch_behind_main_does_not_land_and_an_old_review_does_not_count() {
        let Some(world) = World::new("land-behind") else {
            return;
        };
        world.review("REVIEW: fine\n");
        // Someone else lands first.
        world.git(&["checkout", "-q", "-b", "other", "origin/main"]);
        fs::write(world.work.join("theirs.txt"), "theirs\n").unwrap();
        world.git(&["add", "-A"]);
        world.git(&["commit", "-q", "-m", "theirs"]);
        world.git(&["push", "-q", "origin", "HEAD:refs/heads/main"]);
        world.git(&["checkout", "-q", "work"]);

        let Outcome::Refused(why) = land(&world.work, "origin", PASSED).unwrap() else {
            panic!("it landed behind main");
        };
        assert!(why.contains("merge it in"), "{why}");

        // Merged up, the tree is new: the old review is not of this change.
        world.git(&["merge", "-q", "--no-edit", "origin/main"]);
        let Outcome::Refused(why) = land(&world.work, "origin", PASSED).unwrap() else {
            panic!("it landed unreviewed");
        };
        assert!(why.contains("not reviewed"), "{why}");
    }

    /// A bare `origin.git` whose `main` holds one commit, and a clone of it
    /// on branch `work` with one commit more.
    struct World {
        dir: PathBuf,
        work: PathBuf,
    }

    impl World {
        fn new(name: &str) -> Option<Self> {
            if Command::new("git").arg("--version").output().is_err() {
                return None;
            }
            let id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("{name}-{id}"));
            let work = dir.join("work");
            fs::create_dir_all(&work).unwrap();
            run(&dir, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
            run(&work, &["init", "-q", "-b", "main"]);
            run(&work, &["config", "user.email", "toimija@example.invalid"]);
            run(&work, &["config", "user.name", "Toimija Test"]);
            run(&work, &["remote", "add", "origin", "../origin.git"]);
            fs::write(work.join("README.md"), "# Demo\n").unwrap();
            run(&work, &["add", "-A"]);
            run(&work, &["commit", "-q", "-m", "initial"]);
            run(&work, &["push", "-q", "origin", "main"]);
            run(&work, &["fetch", "-q", "origin"]);
            run(&work, &["checkout", "-q", "-b", "work"]);
            fs::write(work.join("new.txt"), "new\n").unwrap();
            run(&work, &["add", "-A"]);
            run(&work, &["commit", "-q", "-m", "change"]);
            Some(Self { dir, work })
        }

        fn git(&self, args: &[&str]) -> String {
            git(&self.work, args).unwrap()
        }

        fn remote_main(&self) -> String {
            git(&self.dir.join("origin.git"), &["rev-parse", "main"]).unwrap()
        }

        fn review(&self, reply: &'static str) {
            let reviewer = move |_: Agent, _: &str, _: &Path| Ok(reply.to_string());
            review::review(&self.work, "origin/main", &[Agent::Codex], &reviewer)
                .unwrap()
                .unwrap();
        }
    }

    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}
