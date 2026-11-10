//! `toimija graph`: what is cooking in this clone, as the git graph of its
//! branches, each tip with three facts beside it:
//!
//! - **proof**: what sykli's receipts say of the commit (`sykli status
//!   --commit`). Toimija asks; it runs no check.
//! - **review**: the level of the review sykli keeps for the commit's tree
//!   (`refs/notes/sykli/review`), without its findings. A review answers for
//!   the base it was read against, so one read against another commit than
//!   the tip is measured against shows as `stale`.
//! - **who is there**: the open sessions on that branch, in any worktree of
//!   the clone, with their intent.
//!
//! The graph holds the base (`origin/main`) and every local branch not yet in
//! it. Read-only: it writes nothing and asks no network.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Serialize;

use crate::command::CommandRunner;
use crate::sessions::{self, Liveness};

/// Where sykli keeps reviews: one git note per tree (`sykli-review.v1`).
const REVIEW_NOTES: &str = "refs/notes/sykli/review";
/// The longest intent shown beside a session.
const INTENT_WIDTH: usize = 60;

/// What sykli's receipts say of a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum Proof {
    Passed,
    Failed,
    /// sykli holds no receipt of the commit's tree.
    NotRun,
    /// sykli answered with a verdict this view does not name.
    Other(String),
    /// The commit carries no `sykli.json`: nothing gates it.
    NoGate,
    /// sykli could not be asked (not installed, or it gave no answer).
    Unknown(String),
}

/// The review kept for a commit's tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Review {
    Fine,
    Look,
    Blocking,
    /// Blocking, and a person let it cross.
    Overruled,
    /// A review is kept for the tree, but it was read against another base
    /// than the commit this tip is measured against. It says nothing of the
    /// tip as it stands.
    Stale,
    None,
    /// A note is there, but it is not a review this view can read.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Present {
    pub agent: String,
    pub liveness: &'static str,
    pub intent: Option<String>,
    /// The spec the session starts from, when it declared one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    pub worktree: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Tip {
    pub commit: String,
    pub branches: Vec<String>,
    pub subject: String,
    pub proof: Proof,
    pub review: Review,
    pub sessions: Vec<Present>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Line {
    /// A branch tip or the base, after its graph prefix.
    Tip { graph: String, tip: Tip },
    /// Graph edges, or a commit no branch points at.
    Edge { graph: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Graph {
    pub schema: &'static str,
    pub base: Option<String>,
    pub lines: Vec<Line>,
    /// What could not be read; the graph is then partial.
    pub warnings: Vec<String>,
}

/// Asks what sykli says of a commit.
pub type Prover<'a> = &'a dyn Fn(&Path, &str) -> Proof;

pub fn build(
    runner: &dyn CommandRunner,
    repo_root: &Path,
    base: &str,
    prover: Prover<'_>,
) -> Result<Graph> {
    let mut warnings = Vec::new();
    let base_id = git(
        runner,
        repo_root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{base}^{{commit}}"),
        ],
    )
    .ok();
    let base = base_id.as_ref().map(|_| base.to_string());
    if base.is_none() {
        warnings.push("no base ref here: every local branch is shown whole".to_string());
    }
    let mut log = vec![
        "log".to_string(),
        "--graph".to_string(),
        "--simplify-by-decoration".to_string(),
        // Short names, whatever log.decorate says in this clone.
        "--decorate=short".to_string(),
        "--decorate-refs=refs/heads/".to_string(),
        "--format=%x01%H%x02%T%x02%D%x02%s".to_string(),
        "--branches".to_string(),
    ];
    if let Some(base) = &base {
        // The base under its full ref name, however it was given; a base
        // that is no ref (a commit id) has no name to show.
        let full = git(
            runner,
            repo_root,
            &["rev-parse", "--symbolic-full-name", base],
        )
        .unwrap_or_default();
        if !full.is_empty() {
            log.insert(5, format!("--decorate-refs={full}"));
        }
        log.push(base.clone());
        // The base's own history is not what is cooking.
        log.push("--not".to_string());
        log.push(format!("{base}^@"));
    }
    let log: Vec<&str> = log.iter().map(String::as_str).collect();
    let text = git(runner, repo_root, &log)?;

    let reviews = reviews(runner, repo_root, &mut warnings);
    let present = present(runner, repo_root, &mut warnings);
    let lines = text
        .lines()
        .map(|line| {
            let Some((graph, commit)) = line.split_once('\u{1}') else {
                return Line::Edge {
                    graph: line.trim_end().to_string(),
                };
            };
            let mut fields = commit.splitn(4, '\u{2}');
            let (sha, tree, refs, subject) = (
                fields.next().unwrap_or_default(),
                fields.next().unwrap_or_default(),
                fields.next().unwrap_or_default(),
                fields.next().unwrap_or_default(),
            );
            let mut branches = branches(refs);
            if branches.is_empty() && base_id.as_deref() == Some(sha) {
                // The base is a tip under whatever it was named by; a commit
                // id is shown short.
                let named = base.as_deref().unwrap_or(sha);
                branches.push(if sha.starts_with(named) {
                    sha[..sha.len().min(7)].to_string()
                } else {
                    named.to_string()
                });
            }
            if branches.is_empty() {
                // Where branches part: a commit no branch points at.
                return Line::Edge {
                    graph: format!("{graph}{}  (no branch)", &sha[..sha.len().min(7)]),
                };
            }
            let sessions = branches
                .iter()
                .flat_map(|branch| present.get(branch).cloned().unwrap_or_default())
                .collect();
            Line::Tip {
                graph: graph.to_string(),
                tip: Tip {
                    commit: sha.to_string(),
                    proof: prover(repo_root, sha),
                    review: if base_id.as_deref() == Some(sha) {
                        // The base is measured against nothing: its review
                        // is the one it was landed with.
                        review_of(runner, repo_root, reviews.get(tree), Measured::Base)
                    } else {
                        review_of(
                            runner,
                            repo_root,
                            reviews.get(tree),
                            Measured::Against {
                                tip: sha,
                                base: base_id.as_deref(),
                            },
                        )
                    },
                    branches,
                    subject: subject.to_string(),
                    sessions,
                },
            }
        })
        .collect();
    let mut lines: Vec<Line> = lines;
    // A base that no ref names can be simplified out of the log. It is the
    // commit everything here is measured against, so it is shown regardless.
    if let (Some(id), Some(named)) = (&base_id, &base) {
        let shown = lines.iter().any(|line| match line {
            Line::Tip { tip, .. } => &tip.commit == id,
            Line::Edge { .. } => false,
        });
        if !shown {
            let detail = git(runner, repo_root, &["log", "-1", "--format=%T%x02%s", id])?;
            let (tree, subject) = detail.split_once('\u{2}').unwrap_or((&detail, ""));
            let name = if id.starts_with(named.as_str()) {
                id[..id.len().min(7)].to_string()
            } else {
                named.clone()
            };
            lines.push(Line::Tip {
                // Labelled, not drawn: git left it out of the graph, so no
                // position in it would be true.
                graph: "base".to_string(),
                tip: Tip {
                    commit: id.clone(),
                    branches: vec![name],
                    subject: subject.to_string(),
                    proof: prover(repo_root, id),
                    review: review_of(runner, repo_root, reviews.get(tree), Measured::Base),
                    sessions: Vec::new(),
                },
            });
        }
    }
    // A tip whose proof or review could not be read is a gap in the picture.
    for line in &lines {
        let Line::Tip { tip, .. } = line else {
            continue;
        };
        if let Proof::Unknown(why) = &tip.proof {
            let said = format!("proof is unknown: {why}");
            if !warnings.contains(&said) {
                warnings.push(said);
            }
        }
        if tip.review == Review::Unreadable {
            warnings.push(format!(
                "the review kept for {} could not be read",
                tip.branches.join(", ")
            ));
        }
    }
    Ok(Graph {
        schema: "toimija-graph.v1",
        base,
        lines,
        warnings,
    })
}

/// The branch names of a `%D` decoration, the checked-out one included.
fn branches(decoration: &str) -> Vec<String> {
    decoration
        .split(", ")
        .map(|name| name.strip_prefix("HEAD -> ").unwrap_or(name).trim())
        .filter(|name| !name.is_empty() && *name != "HEAD")
        .map(str::to_string)
        .collect()
}

/// Every tree that has a review note, with the note's blob.
fn reviews(
    runner: &dyn CommandRunner,
    repo_root: &Path,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, String> {
    if git(
        runner,
        repo_root,
        &["rev-parse", "--verify", "--quiet", REVIEW_NOTES],
    )
    .is_err()
    {
        return BTreeMap::new();
    }
    match git(runner, repo_root, &["notes", "--ref", REVIEW_NOTES, "list"]) {
        Ok(list) => list
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(blob, tree)| (tree.to_string(), blob.to_string()))
            .collect(),
        Err(error) => {
            warnings.push(format!("the reviews could not be listed: {error}"));
            BTreeMap::new()
        }
    }
}

/// What a tip's review is held against.
enum Measured<'a> {
    /// The base itself: nothing to measure it against.
    Base,
    /// A tip, against where it and the base part (their merge base).
    Against { tip: &'a str, base: Option<&'a str> },
}

/// The review kept for a tree, read from its note. A tip without a note costs
/// no git process; one with a note costs two, the note and the merge base.
fn review_of(
    runner: &dyn CommandRunner,
    repo_root: &Path,
    blob: Option<&String>,
    measured: Measured<'_>,
) -> Review {
    let Some(blob) = blob else {
        return Review::None;
    };
    let Ok(text) = git(runner, repo_root, &["cat-file", "blob", blob]) else {
        return Review::Unreadable;
    };
    let Ok(note) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Review::Unreadable;
    };
    let level = (note["level"].as_str(), note["overruled"].is_null());
    if !matches!(level.0, Some("fine" | "look" | "blocking")) {
        return Review::Unreadable;
    }
    if let Measured::Against { tip, base } = measured {
        // A review answers only for the base it was read against.
        let Some(read_against) = note["base"].as_str() else {
            return Review::Unreadable;
        };
        // With no base there is nothing the review could have been read
        // against here. Otherwise git says where the two part: exit 1 is a
        // known answer (nothing in common), anything else is not an answer.
        let parts_at = match base {
            None => None,
            Some(base) => match runner.run(repo_root, "git", &["merge-base", tip, base]) {
                Ok(output) if output.status == 0 => {
                    Some(output.stdout_lossy().trim_end().to_string())
                }
                Ok(output) if output.status == 1 => None,
                Ok(_) | Err(_) => return Review::Unreadable,
            },
        };
        if parts_at.as_deref() != Some(read_against) {
            return Review::Stale;
        }
    }
    match level {
        (Some("fine"), _) => Review::Fine,
        (Some("look"), _) => Review::Look,
        (Some("blocking"), true) => Review::Blocking,
        (Some("blocking"), false) => Review::Overruled,
        _ => Review::Unreadable,
    }
}

/// The open sessions of every worktree of the clone, by branch.
fn present(
    runner: &dyn CommandRunner,
    repo_root: &Path,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, Vec<Present>> {
    let mut by_branch: BTreeMap<String, Vec<Present>> = BTreeMap::new();
    let worktrees = match git(
        runner,
        repo_root,
        &["worktree", "list", "--porcelain", "-z"],
    ) {
        // NUL-separated: git quotes nothing, whatever the path holds.
        Ok(list) => list
            .split('\0')
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(PathBuf::from)
            .collect(),
        Err(error) => {
            warnings.push(format!("the worktrees could not be listed: {error}"));
            vec![repo_root.to_path_buf()]
        }
    };
    for worktree in worktrees {
        // No sessions folder is no session; one that cannot be looked at is
        // a gap, since a live session may sit behind it.
        match std::fs::metadata(worktree.join(".toimija/sessions")) {
            Ok(found) if found.is_dir() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                warnings.push(format!(
                    "the sessions of {} could not be looked at: {error}",
                    worktree.display()
                ));
                continue;
            }
        }
        let report = match sessions::report(runner, &worktree, sessions::DEFAULT_STALE_AFTER_SECS) {
            Ok(report) => report,
            Err(error) => {
                warnings.push(format!(
                    "the sessions of {} could not be read: {error}",
                    worktree.display()
                ));
                continue;
            }
        };
        if report.coverage.status != crate::coverage::CoverageStatus::Available {
            // A marker that could not be read may be a live session.
            warnings.push(format!(
                "the sessions of {} were read only in part: {}",
                worktree.display(),
                report
                    .coverage
                    .reason
                    .as_deref()
                    .unwrap_or("a session marker could not be read")
            ));
        }
        for view in report.sessions {
            let liveness = match view.liveness {
                Liveness::Active => "live",
                Liveness::Stale => "stale",
                Liveness::Closed => continue,
            };
            let Some(branch) = view.marker.branch.clone() else {
                continue;
            };
            by_branch.entry(branch).or_default().push(Present {
                agent: view.agent_label().to_string(),
                liveness,
                intent: view.marker.intent.clone(),
                spec: view.marker.spec.clone(),
                worktree: view.marker.worktree.clone(),
            });
        }
    }
    by_branch
}

/// What sykli says of a commit, asked of the installed sykli.
pub fn sykli_proof(repo_root: &Path, commit: &str) -> Proof {
    proof_from("sykli", repo_root, commit)
}

fn proof_from(sykli: &str, repo_root: &Path, commit: &str) -> Proof {
    let mut asked = std::process::Command::new(sykli);
    asked
        .current_dir(repo_root)
        .args(["status", "--commit", commit, "--json"]);
    // As for toimija's own git: no inherited GIT_DIR points sykli elsewhere.
    crate::command::scrub_git_location_env(&mut asked);
    let output = match asked.output() {
        Ok(output) => output,
        // Without sykli the one thing still known is whether the commit
        // carries a contract at all: one that does not is gated by nothing.
        Err(_) if !carries_contract(repo_root, commit) => return Proof::NoGate,
        Err(_) => return Proof::Unknown("sykli is not installed".to_string()),
    };
    let Ok(status) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        let said = String::from_utf8_lossy(&output.stderr);
        if said.contains("has no sykli.json") {
            return Proof::NoGate;
        }
        return Proof::Unknown("sykli gave no answer".to_string());
    };
    proof_of(&status)
}

/// The proof in sykli's answer. A pass stands on its receipt; any other
/// verdict is only as good as the runs sykli could read.
fn proof_of(status: &serde_json::Value) -> Proof {
    let unread = status["unreadable_runs"].as_array().map_or(0, Vec::len);
    match status["verdict"].as_str() {
        Some("passed") => Proof::Passed,
        Some(_) if unread > 0 => {
            Proof::Unknown(format!("sykli could not read {unread} of its runs"))
        }
        Some("failed") => Proof::Failed,
        Some("none") => Proof::NotRun,
        Some(other) => Proof::Other(other.to_string()),
        None => Proof::Unknown("sykli gave no verdict".to_string()),
    }
}

fn carries_contract(repo_root: &Path, commit: &str) -> bool {
    ["sykli.json", "sykli.rs"].iter().any(|contract| {
        // Pinned to this repository: no inherited GIT_DIR answers for another.
        crate::command::git_command(repo_root)
            .args(["cat-file", "-e", &format!("{commit}:{contract}")])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

pub fn render_text(graph: &Graph) -> String {
    let mut out = String::new();
    let tips: Vec<&Tip> = graph
        .lines
        .iter()
        .filter_map(|line| match line {
            Line::Tip { tip, .. } => Some(tip),
            Line::Edge { .. } => None,
        })
        .collect();
    let names = |tip: &Tip| tip.branches.join(", ");
    let name_width = tips.iter().map(|tip| names(tip).len()).max().unwrap_or(0);
    let graph_width = graph
        .lines
        .iter()
        // Only tips are padded: an edge line carries its own text.
        .filter_map(|line| match line {
            Line::Tip { graph, .. } => Some(graph.trim_end().chars().count()),
            Line::Edge { .. } => None,
        })
        .max()
        .unwrap_or(0);
    for line in &graph.lines {
        match line {
            Line::Edge { graph } => {
                let _ = writeln!(out, "{graph}");
            }
            Line::Tip { graph, tip } => {
                let _ = write!(
                    out,
                    "{:<graph_width$} {}  {:<name_width$}  {:<8}  review: {:<9}",
                    graph.trim_end(),
                    &tip.commit[..tip.commit.len().min(7)],
                    names(tip),
                    proof_word(&tip.proof),
                    review_word(&tip.review),
                );
                for (index, session) in tip.sessions.iter().enumerate() {
                    out.push_str(if index == 0 { "  " } else { "; " });
                    let _ = write!(out, "{} {}", session.agent, session.liveness);
                    match &session.intent {
                        Some(intent) => {
                            let _ = write!(out, ": \"{}\"", shorten(intent));
                        }
                        None => out.push_str(": intent not given"),
                    }
                    if let Some(spec) = &session.spec {
                        let _ = write!(out, " [spec: {spec}]");
                    }
                }
                out.truncate(out.trim_end().len());
                out.push('\n');
            }
        }
    }
    if tips.is_empty() {
        out.push_str("No branches here.\n");
    }
    for warning in &graph.warnings {
        let _ = writeln!(out, "warning: {warning}");
    }
    out
}

fn proof_word(proof: &Proof) -> &str {
    match proof {
        Proof::Passed => "proven",
        Proof::Failed => "FAILED",
        Proof::NotRun => "not run",
        Proof::Other(word) => word,
        Proof::NoGate => "no gate",
        Proof::Unknown(_) => "unknown",
    }
}

fn review_word(review: &Review) -> &'static str {
    match review {
        Review::Fine => "fine",
        Review::Look => "look",
        Review::Blocking => "BLOCKING",
        Review::Overruled => "overruled",
        Review::Stale => "stale",
        Review::None => "none",
        Review::Unreadable => "unreadable",
    }
}

fn shorten(intent: &str) -> String {
    let line = intent.lines().next().unwrap_or_default();
    match line.char_indices().nth(INTENT_WIDTH) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.to_string(),
    }
}

fn git(runner: &dyn CommandRunner, repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = runner.run(repo_root, "git", args)?;
    if output.status != 0 {
        bail!("git {}: {}", args.join(" "), output.stderr_lossy().trim());
    }
    Ok(output.stdout_lossy().trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::ProcessCommandRunner;
    use std::fs;

    fn run(root: &Path, args: &[&str]) -> String {
        git(&ProcessCommandRunner, root, args).unwrap()
    }

    /// `origin/main` at the first commit; `work` one commit ahead of it and
    /// checked out; `other` one different commit ahead.
    fn repo() -> Option<tempfile::TempDir> {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run(root, &["init", "-q", "-b", "main"]);
        run(root, &["config", "user.email", "toimija@example.invalid"]);
        run(root, &["config", "user.name", "Toimija Test"]);
        fs::write(root.join("a.txt"), "a\n").unwrap();
        run(root, &["add", "-A"]);
        run(root, &["commit", "-q", "-m", "first"]);
        run(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        run(root, &["checkout", "-q", "-b", "other"]);
        fs::write(root.join("o.txt"), "o\n").unwrap();
        run(root, &["add", "-A"]);
        run(root, &["commit", "-q", "-m", "other work"]);
        run(root, &["checkout", "-q", "-b", "work", "main"]);
        fs::write(root.join("w.txt"), "w\n").unwrap();
        run(root, &["add", "-A"]);
        run(root, &["commit", "-q", "-m", "the work"]);
        Some(dir)
    }

    fn tips(graph: &Graph) -> Vec<&Tip> {
        graph
            .lines
            .iter()
            .filter_map(|line| match line {
                Line::Tip { tip, .. } => Some(tip),
                Line::Edge { .. } => None,
            })
            .collect()
    }

    fn tip<'a>(graph: &'a Graph, branch: &str) -> &'a Tip {
        tips(graph)
            .into_iter()
            .find(|tip| tip.branches.iter().any(|name| name == branch))
            .unwrap_or_else(|| panic!("no tip for {branch}"))
    }

    #[test]
    fn every_branch_not_in_the_base_is_a_tip_with_its_proof_and_review() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        let work_tree = run(root, &["rev-parse", "work^{tree}"]);
        let other_tree = run(root, &["rev-parse", "other^{tree}"]);
        let note = |tree: &str, json: &str| {
            run(
                root,
                &[
                    "notes",
                    "--ref",
                    REVIEW_NOTES,
                    "add",
                    "-f",
                    "-m",
                    json,
                    tree,
                ],
            );
        };
        let base = run(root, &["rev-parse", "origin/main"]);
        note(
            &work_tree,
            &format!(r#"{{"level":"look","base":"{base}","overruled":null}}"#),
        );
        note(
            &other_tree,
            &format!(
                r#"{{"level":"blocking","base":"{base}","overruled":{{"why":"accepted","at":1}}}}"#
            ),
        );
        let work = run(root, &["rev-parse", "work"]);
        let prover = |_: &Path, commit: &str| {
            if commit == work {
                Proof::Passed
            } else {
                Proof::NotRun
            }
        };

        let graph = build(&ProcessCommandRunner, root, "origin/main", &prover).unwrap();
        assert_eq!(tips(&graph).len(), 3, "{graph:#?}");
        assert_eq!(tip(&graph, "work").proof, Proof::Passed);
        assert_eq!(tip(&graph, "work").review, Review::Look);
        assert_eq!(tip(&graph, "other").proof, Proof::NotRun);
        assert_eq!(tip(&graph, "other").review, Review::Overruled);
        let base = tip(&graph, "origin/main");
        assert!(base.branches.contains(&"main".to_string()), "{base:?}");
        assert_eq!(base.review, Review::None);

        let text = render_text(&graph);
        assert!(text.contains("proven"), "{text}");
        assert!(text.contains("review: look"), "{text}");
        assert!(text.contains("review: overruled"), "{text}");
        assert!(text.contains("not run"), "{text}");
        assert!(
            !text.contains("accepted"),
            "no review detail is shown: {text}"
        );
    }

    #[test]
    fn a_blocking_review_and_a_failed_proof_stand_out_and_a_bad_note_is_not_a_review() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        let work_tree = run(root, &["rev-parse", "work^{tree}"]);
        let other_tree = run(root, &["rev-parse", "other^{tree}"]);
        run(
            root,
            &[
                "notes",
                "--ref",
                REVIEW_NOTES,
                "add",
                "-m",
                &format!(
                    r#"{{"level":"blocking","base":"{}","overruled":null}}"#,
                    run(root, &["rev-parse", "origin/main"])
                ),
                &work_tree,
            ],
        );
        run(
            root,
            &[
                "notes",
                "--ref",
                REVIEW_NOTES,
                "add",
                "-m",
                "not json",
                &other_tree,
            ],
        );
        let failed = |_: &Path, _: &str| Proof::Failed;
        let graph = build(&ProcessCommandRunner, root, "origin/main", &failed).unwrap();
        assert_eq!(tip(&graph, "work").review, Review::Blocking);
        assert_eq!(tip(&graph, "other").review, Review::Unreadable);
        let text = render_text(&graph);
        assert!(text.contains("review: BLOCKING"), "{text}");
        assert!(text.contains("FAILED"), "{text}");
        assert!(text.contains("review: unreadable"), "{text}");
    }

    /// Reviews are sykli's: one note per tree under its ref, and no other ref
    /// is read.
    #[test]
    fn reviews_are_read_from_syklis_notes_and_from_no_other_ref() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        let base = run(root, &["rev-parse", "origin/main"]);
        let fine = format!(
            r#"{{"schema":"sykli-review.v1","level":"fine","base":"{base}","overruled":null}}"#
        );
        let note = |notes: &str, branch: &str| {
            let tree = run(root, &["rev-parse", &format!("{branch}^{{tree}}")]);
            run(root, &["notes", "--ref", notes, "add", "-m", &fine, &tree]);
        };
        note("refs/notes/sykli/review", "work");
        note("refs/notes/toimija/review", "other");
        let none = |_: &Path, _: &str| Proof::NotRun;
        let graph = build(&ProcessCommandRunner, root, "origin/main", &none).unwrap();
        assert_eq!(tip(&graph, "work").review, Review::Fine);
        assert_eq!(tip(&graph, "other").review, Review::None);
    }

    /// A review answers for the base it was read against. Where the tip is
    /// now measured against another commit, the review is stale, whatever it
    /// said, and is never shown as its level.
    #[test]
    fn a_review_read_against_another_base_is_stale_never_its_level() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        let note = |branch: &str, json: &str| {
            let tree = run(root, &["rev-parse", &format!("{branch}^{{tree}}")]);
            run(
                root,
                &["notes", "--ref", REVIEW_NOTES, "add", "-m", json, &tree],
            );
        };
        // Read against a commit that is not where `work` and the base part.
        let elsewhere = run(root, &["rev-parse", "other"]);
        note(
            "work",
            &format!(r#"{{"level":"fine","base":"{elsewhere}","overruled":null}}"#),
        );
        note(
            "other",
            &format!(
                r#"{{"level":"blocking","base":"{elsewhere}","overruled":{{"why":"x","at":1}}}}"#
            ),
        );
        let none = |_: &Path, _: &str| Proof::NotRun;
        let graph = build(&ProcessCommandRunner, root, "origin/main", &none).unwrap();
        assert_eq!(tip(&graph, "work").review, Review::Stale);
        assert_eq!(tip(&graph, "other").review, Review::Stale);
        let text = render_text(&graph);
        assert!(text.contains("review: stale"), "{text}");
        assert!(!text.contains("review: fine"), "{text}");
        assert!(!text.contains("overruled"), "{text}");
        let json = serde_json::to_string(&graph).unwrap();
        assert!(json.contains(r#""review":"stale""#), "{json}");

        // The base moves on: a review that held for the earlier base is stale.
        let first = run(root, &["rev-parse", "origin/main"]);
        let tree = run(root, &["rev-parse", "work^{tree}"]);
        run(
            root,
            &[
                "notes",
                "--ref",
                REVIEW_NOTES,
                "add",
                "-f",
                "-m",
                &format!(r#"{{"level":"fine","base":"{first}","overruled":null}}"#),
                &tree,
            ],
        );
        let graph = build(&ProcessCommandRunner, root, "origin/main", &none).unwrap();
        assert_eq!(tip(&graph, "work").review, Review::Fine);
        run(root, &["checkout", "-q", "work"]);
        run(root, &["merge", "-q", "--no-edit", "other"]);
        run(root, &["update-ref", "refs/remotes/origin/main", "other"]);
        let merged_tree = run(root, &["rev-parse", "work^{tree}"]);
        run(
            root,
            &[
                "notes",
                "--ref",
                REVIEW_NOTES,
                "add",
                "-f",
                "-m",
                &format!(r#"{{"level":"fine","base":"{first}","overruled":null}}"#),
                &merged_tree,
            ],
        );
        let graph = build(&ProcessCommandRunner, root, "origin/main", &none).unwrap();
        assert_eq!(tip(&graph, "work").review, Review::Stale);

        // A note that names no base is not a review this view can place.
        run(
            root,
            &[
                "notes",
                "--ref",
                REVIEW_NOTES,
                "add",
                "-f",
                "-m",
                r#"{"level":"fine","overruled":null}"#,
                &merged_tree,
            ],
        );
        let graph = build(&ProcessCommandRunner, root, "origin/main", &none).unwrap();
        assert_eq!(tip(&graph, "work").review, Review::Unreadable);
    }

    /// A git that fails when asked where a tip and the base part.
    struct NoMergeBase;

    impl CommandRunner for NoMergeBase {
        fn run(
            &self,
            cwd: &Path,
            program: &str,
            args: &[&str],
        ) -> std::io::Result<crate::command::CommandOutput> {
            if args.first() == Some(&"merge-base") {
                return Ok(crate::command::CommandOutput {
                    status: 128,
                    stdout: Vec::new(),
                    stderr: b"fatal: could not read".to_vec(),
                });
            }
            ProcessCommandRunner.run(cwd, program, args)
        }
    }

    /// Where the comparison itself cannot be made, the review is not called
    /// stale: it is unreadable, with the warning that makes the graph partial.
    /// Two histories with nothing in common are another matter: there the
    /// answer is known, and the review is stale.
    #[test]
    fn a_base_that_cannot_be_compared_is_unreadable_not_stale() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        let base = run(root, &["rev-parse", "origin/main"]);
        let tree = run(root, &["rev-parse", "work^{tree}"]);
        run(
            root,
            &[
                "notes",
                "--ref",
                REVIEW_NOTES,
                "add",
                "-m",
                &format!(r#"{{"level":"fine","base":"{base}","overruled":null}}"#),
                &tree,
            ],
        );
        let none = |_: &Path, _: &str| Proof::NotRun;
        let graph = build(&NoMergeBase, root, "origin/main", &none).unwrap();
        assert_eq!(tip(&graph, "work").review, Review::Unreadable);
        assert!(
            graph
                .warnings
                .iter()
                .any(|warning| warning.contains("work")),
            "{:?}",
            graph.warnings
        );

        // An orphan branch shares no commit with the base.
        run(root, &["checkout", "-q", "--orphan", "apart"]);
        run(root, &["commit", "-q", "--allow-empty", "-m", "apart"]);
        let apart_tree = run(root, &["rev-parse", "apart^{tree}"]);
        run(
            root,
            &[
                "notes",
                "--ref",
                REVIEW_NOTES,
                "add",
                "-f",
                "-m",
                &format!(r#"{{"level":"fine","base":"{base}","overruled":null}}"#),
                &apart_tree,
            ],
        );
        let graph = build(&ProcessCommandRunner, root, "origin/main", &none).unwrap();
        assert_eq!(tip(&graph, "apart").review, Review::Stale);
    }

    #[test]
    fn a_branch_already_in_the_base_is_not_in_progress() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        // The base moves on to `other`; `main` is now behind it, inside it.
        run(root, &["update-ref", "refs/remotes/origin/main", "other"]);
        let none = |_: &Path, _: &str| Proof::NotRun;
        let graph = build(&ProcessCommandRunner, root, "origin/main", &none).unwrap();
        let shown: Vec<&String> = tips(&graph).iter().flat_map(|tip| &tip.branches).collect();
        assert!(!shown.iter().any(|name| *name == "main"), "{shown:?}");
        assert!(shown.iter().any(|name| *name == "work"), "{shown:?}");
        assert!(shown.iter().any(|name| *name == "other"), "{shown:?}");
    }

    #[test]
    fn an_unreadable_session_marker_is_a_warning_not_silence() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        fs::create_dir_all(root.join(".toimija/sessions")).unwrap();
        fs::write(root.join(".toimija/sessions/broken.json"), "not a marker").unwrap();
        let none = |_: &Path, _: &str| Proof::NotRun;
        let graph = build(&ProcessCommandRunner, root, "origin/main", &none).unwrap();
        assert!(
            graph
                .warnings
                .iter()
                .any(|w| w.contains("read only in part")),
            "{:?}",
            graph.warnings
        );
    }

    #[test]
    fn the_base_is_shown_however_it_is_named() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        // No local branch sits on the base, and it is named by its full ref.
        run(root, &["checkout", "-q", "work"]);
        run(root, &["branch", "-q", "-D", "main"]);
        let none = |_: &Path, _: &str| Proof::Passed;
        for name in ["refs/remotes/origin/main", "origin/main"] {
            let graph = build(&ProcessCommandRunner, root, name, &none).unwrap();
            assert_eq!(tip(&graph, "origin/main").proof, Proof::Passed, "{name}");
        }
    }

    #[test]
    fn an_unknown_proof_and_an_unreadable_review_are_warnings() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        let work_tree = run(root, &["rev-parse", "work^{tree}"]);
        run(
            root,
            &[
                "notes",
                "--ref",
                REVIEW_NOTES,
                "add",
                "-m",
                "not json",
                &work_tree,
            ],
        );
        let unknown = |_: &Path, _: &str| Proof::Unknown("sykli is not installed".to_string());
        let graph = build(&ProcessCommandRunner, root, "origin/main", &unknown).unwrap();
        let all = graph.warnings.join("\n");
        assert!(all.contains("sykli is not installed"), "{all}");
        assert_eq!(
            all.matches("sykli is not installed").count(),
            1,
            "said once: {all}"
        );
        assert!(
            all.contains("the review kept for work could not be read"),
            "{all}"
        );
    }

    #[test]
    fn without_sykli_a_commit_with_no_contract_has_no_gate_and_one_with_it_is_unknown() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        let absent = "sykli-that-is-not-installed";
        assert_eq!(proof_from(absent, root, "work"), Proof::NoGate);
        fs::write(root.join("sykli.json"), "{}\n").unwrap();
        run(root, &["add", "-A"]);
        run(root, &["commit", "-q", "-m", "a gate"]);
        assert_eq!(
            proof_from(absent, root, "work"),
            Proof::Unknown("sykli is not installed".to_string())
        );
    }

    #[test]
    fn a_base_given_as_a_commit_id_is_still_a_tip_with_its_proof() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        run(root, &["checkout", "-q", "work"]);
        run(root, &["branch", "-q", "-D", "main"]);
        let id = run(root, &["rev-parse", "origin/main"]);
        let passed = |_: &Path, _: &str| Proof::Passed;
        let graph = build(&ProcessCommandRunner, root, &id, &passed).unwrap();
        let base = tips(&graph)
            .into_iter()
            .find(|tip| tip.commit == id)
            .expect("the base is a tip");
        assert_eq!(base.proof, Proof::Passed);
        assert_eq!(base.branches, [id[..7].to_string()]);
    }

    #[test]
    fn a_worktree_with_an_unusual_path_is_still_read() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        let odd = dir.path().join("wörk \"tree\"");
        run(
            root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "odd",
                &odd.to_string_lossy(),
                "main",
            ],
        );
        fs::create_dir_all(odd.join(".toimija/sessions")).unwrap();
        fs::write(odd.join(".toimija/sessions/broken.json"), "not a marker").unwrap();
        let none = |_: &Path, _: &str| Proof::NotRun;
        let graph = build(&ProcessCommandRunner, root, "origin/main", &none).unwrap();
        // Its sessions were looked at: the unreadable marker there is named.
        assert!(
            graph.warnings.iter().any(|w| w.contains("wörk")),
            "{:?}",
            graph.warnings
        );
    }

    #[test]
    fn branch_names_are_short_whatever_git_is_configured_to_print() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        run(root, &["config", "log.decorate", "full"]);
        let none = |_: &Path, _: &str| Proof::NotRun;
        let graph = build(&ProcessCommandRunner, root, "origin/main", &none).unwrap();
        assert_eq!(tip(&graph, "work").subject, "the work");
        assert_eq!(tip(&graph, "origin/main").subject, "first");
    }

    #[test]
    fn a_verdict_short_of_passed_is_unknown_when_sykli_could_not_read_all_its_runs() {
        let answer = |verdict: &str, unread: usize| serde_json::json!({"verdict": verdict, "unreadable_runs": vec!["r"; unread]});
        assert_eq!(proof_of(&answer("passed", 2)), Proof::Passed);
        assert_eq!(proof_of(&answer("none", 0)), Proof::NotRun);
        assert_eq!(proof_of(&answer("failed", 0)), Proof::Failed);
        for verdict in ["none", "failed"] {
            assert_eq!(
                proof_of(&answer(verdict, 2)),
                Proof::Unknown("sykli could not read 2 of its runs".to_string())
            );
        }
        assert!(matches!(
            proof_of(&serde_json::json!({})),
            Proof::Unknown(_)
        ));
    }

    #[test]
    fn tips_line_up_right_after_the_graph_and_edges_do_not_widen_it() {
        let tip = |graph: &str, name: &str| Line::Tip {
            graph: graph.to_string(),
            tip: Tip {
                commit: "abcdef0123".to_string(),
                branches: vec![name.to_string()],
                subject: String::new(),
                proof: Proof::Passed,
                review: Review::None,
                sessions: Vec::new(),
            },
        };
        let graph = Graph {
            schema: "toimija-graph.v1",
            base: None,
            lines: vec![
                tip("* ", "work"),
                tip("| * ", "other"),
                Line::Edge {
                    graph: "* 1234567  (no branch)".to_string(),
                },
            ],
            warnings: Vec::new(),
        };
        let text = render_text(&graph);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("*   abcdef0  work "), "{text}");
        assert!(lines[1].starts_with("| * abcdef0  other"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn a_sessions_folder_that_cannot_be_looked_at_is_a_warning() {
        use std::os::unix::fs::PermissionsExt;
        let Some(dir) = repo() else { return };
        let root = dir.path();
        let hidden = root.join(".toimija");
        fs::create_dir_all(hidden.join("sessions")).unwrap();
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).unwrap();
        let none = |_: &Path, _: &str| Proof::NotRun;
        let graph = build(&ProcessCommandRunner, root, "origin/main", &none);
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o755)).unwrap();
        let graph = graph.unwrap();
        // Root reads through any mode: only assert where the denial is real.
        if runs_as_root() {
            return;
        }
        assert!(
            graph
                .warnings
                .iter()
                .any(|w| w.contains("could not be looked at")),
            "{:?}",
            graph.warnings
        );
    }

    #[cfg(unix)]
    fn runs_as_root() -> bool {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "0")
    }

    #[test]
    fn a_commit_id_base_that_nothing_points_at_is_still_shown() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        // `work` gets a second commit; the base is its first, which no ref names
        // and which is neither a root nor where branches part.
        let base = run(root, &["rev-parse", "work"]);
        fs::write(root.join("w2.txt"), "w2\n").unwrap();
        run(root, &["add", "-A"]);
        run(root, &["commit", "-q", "-m", "more work"]);
        let passed = |_: &Path, _: &str| Proof::Passed;
        let graph = build(&ProcessCommandRunner, root, &base, &passed).unwrap();
        let shown = tips(&graph)
            .into_iter()
            .find(|tip| tip.commit == base)
            .expect("the base is a tip");
        assert_eq!(shown.proof, Proof::Passed);
        assert_eq!(shown.subject, "the work");
        // It is not drawn as a node of the graph: git did not place it there.
        let text = render_text(&graph);
        let line = text.lines().find(|line| line.contains(&base[..7])).unwrap();
        assert!(line.starts_with("base"), "{text}");
    }

    #[test]
    fn without_a_base_every_branch_is_shown_however_many_there_are() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        for n in 0..55 {
            let name = format!("b{n}");
            run(root, &["checkout", "-q", "-b", &name, "main"]);
            run(root, &["commit", "-q", "--allow-empty", "-m", &name]);
        }
        let none = |_: &Path, _: &str| Proof::NotRun;
        let graph = build(&ProcessCommandRunner, root, "origin/gone", &none).unwrap();
        let shown: Vec<&String> = tips(&graph).iter().flat_map(|tip| &tip.branches).collect();
        for n in 0..55 {
            assert!(shown.contains(&&format!("b{n}")), "b{n} is missing");
        }
    }

    #[test]
    fn without_the_base_ref_the_graph_says_so_and_still_shows_the_branches() {
        let Some(dir) = repo() else { return };
        let root = dir.path();
        let none = |_: &Path, _: &str| Proof::NoGate;
        let graph = build(&ProcessCommandRunner, root, "origin/trunk", &none).unwrap();
        assert_eq!(graph.base, None);
        assert!(graph.warnings[0].contains("no base ref"), "{graph:?}");
        assert!(tips(&graph).len() >= 2);
        assert!(render_text(&graph).contains("warning: no base ref"));
    }

    #[test]
    fn a_long_intent_is_cut_and_only_its_first_line_is_shown() {
        assert_eq!(shorten("fix the watcher\nand more"), "fix the watcher");
        let long = "x".repeat(INTENT_WIDTH + 5);
        assert_eq!(shorten(&long).chars().count(), INTENT_WIDTH + 1);
    }
}
