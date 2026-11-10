//! `cargo xtask review`: a fresh agent reviews this branch and says how
//! serious what it found is.
//!
//! It is the one task of `review.json`, so `sykli run review.json` gives the
//! review a receipt bound to the tree it read. The reviewer is another agent
//! than the one that wrote the branch where one is installed, started with
//! read-only tools and none of the author's conversation.
//!
//! Exit codes:
//!
//! - 0: `fine`, `look`, a blocking review a person overruled, or nothing to
//!   review (the commit is already in the base).
//! - 1: `blocking`.
//! - 2: no review happened: uncommitted work, no reviewer installed, a
//!   reviewer that failed, ran out of time or gave no verdict. Never a pass.
//!
//! What a review found is kept for the commit's tree in the clone's common
//! git directory (`toimija/reviews/<tree>.json`), outside every worktree.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The ref a branch is reviewed against.
const BASE: &str = "origin/main";
/// How long a reviewer may take before it counts as no review.
const TIME_LIMIT: Duration = Duration::from_secs(20 * 60);
const SCHEMA: &str = "toimija-review.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Fine,
    Look,
    Blocking,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Fine => "fine",
            Self::Look => "look",
            Self::Blocking => "blocking",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Codex,
    Claude,
}

impl Agent {
    /// Reviewers in the order they are tried.
    const ALL: [Self; 2] = [Self::Codex, Self::Claude];

    fn program(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.program())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub level: Level,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub level: Level,
    pub findings: Vec<Finding>,
}

/// Whether the reviewer is another agent than the branch's authors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Independent {
    Yes,
    No,
    /// No commit of the branch names the agent that wrote it.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overruled {
    pub why: String,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub schema: String,
    pub tree: String,
    pub head: String,
    pub base: String,
    pub level: Level,
    pub findings: Vec<Finding>,
    pub reviewer: Agent,
    pub authors: Vec<Agent>,
    pub independent: Independent,
    pub at: u64,
    pub overruled: Option<Overruled>,
}

/// What the reviewer's reply says. The verdict is its last `REVIEW:` line;
/// a reply without one, or with a level that is not one of the three, is no
/// review. A verdict milder than its own worst finding is raised to it.
pub fn parse_verdict(reply: &str) -> Result<Verdict> {
    let mut stated = None;
    let mut findings = Vec::new();
    for line in reply.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("REVIEW:") {
            let word = rest
                .trim()
                .trim_matches(|c| c == '*' || c == '`' || c == '.');
            stated = Some(match word.to_ascii_lowercase().as_str() {
                "fine" => Level::Fine,
                "look" => Level::Look,
                "blocking" => Level::Blocking,
                other => bail!("the reviewer's verdict `{other}` is not fine, look or blocking"),
            });
        } else if let Some(text) = line.strip_prefix("- [blocking]") {
            findings.push(finding(Level::Blocking, text));
        } else if let Some(text) = line.strip_prefix("- [look]") {
            findings.push(finding(Level::Look, text));
        }
    }
    let Some(stated) = stated else {
        bail!("the reviewer gave no `REVIEW:` verdict line");
    };
    let worst = findings
        .iter()
        .map(|f| f.level)
        .max()
        .unwrap_or(Level::Fine);
    Ok(Verdict {
        level: stated.max(worst),
        findings,
    })
}

fn finding(level: Level, text: &str) -> Finding {
    Finding {
        level,
        text: text.trim().to_string(),
    }
}

/// The agents the branch's commits name as co-authors.
pub fn authors(trailers: &str) -> BTreeSet<Agent> {
    let lower = trailers.to_ascii_lowercase();
    let mut found = BTreeSet::new();
    if lower.contains("claude") {
        found.insert(Agent::Claude);
    }
    if lower.contains("codex") || lower.contains("openai") {
        found.insert(Agent::Codex);
    }
    found
}

/// The reviewer: the first installed agent that did not write the branch,
/// else the first installed one, in a fresh session.
pub fn choose(authors: &BTreeSet<Agent>, installed: &[Agent]) -> Option<(Agent, Independent)> {
    if authors.is_empty() {
        return installed
            .first()
            .map(|agent| (*agent, Independent::Unknown));
    }
    installed
        .iter()
        .find(|agent| !authors.contains(agent))
        .map(|agent| (*agent, Independent::Yes))
        .or_else(|| installed.first().map(|agent| (*agent, Independent::No)))
}

/// 0 when the push may cross, 1 when a blocking review stands.
pub fn exit_code(record: &Record) -> u8 {
    u8::from(record.level == Level::Blocking && record.overruled.is_none())
}

pub fn prompt(base: &str, head: &str) -> String {
    format!(
        "You are reviewing a change in this repository. You did not write it.\n\
         \n\
         The change is every commit in `{base}..{head}`. Read it with \
         `git diff {base}...{head}` and `git log {base}..{head}`, and read the files it \
         touches as far as you need to judge it. Change nothing: no edits, no commits, \
         no pushes.\n\
         \n\
         Judge only what would matter to the person who owns this repository:\n\
         - blocking: it is wrong or harmful as written. A bug the change introduces, \
         lost or corrupted data, a check loosened to pass, a secret, a broken rule of \
         this repository's AGENTS.md.\n\
         - look: it works, but a person should know about it before it lands. A risky \
         design choice, a missing test for new behaviour, text that no longer matches \
         the code.\n\
         Style, naming and things you would merely have done differently are not \
         findings.\n\
         \n\
         Reply with one line per finding, each starting with `- [blocking]` or \
         `- [look]`, then the file and line, then one sentence. End with exactly one \
         line: `REVIEW: fine`, `REVIEW: look` or `REVIEW: blocking`. With no findings, \
         reply with only `REVIEW: fine`.\n"
    )
}

/// Runs the reviewer on a prompt and returns its reply.
pub type Reviewer<'a> = &'a dyn Fn(Agent, &str, &Path) -> Result<String>;

/// Reviews the commit checked out at `root`, or reuses what this clone keeps
/// for its tree. Returns `None` when the commit is already in the base.
pub fn review(root: &Path, installed: &[Agent], reviewer: Reviewer<'_>) -> Result<Option<Record>> {
    if !git(root, &["status", "--porcelain"])?.is_empty() {
        bail!("uncommitted or untracked files here: a review is of a commit, so commit first");
    }
    let head = git(root, &["rev-parse", "HEAD"])?;
    let tree = git(root, &["rev-parse", "HEAD^{tree}"])?;
    let base = git(root, &["merge-base", BASE, "HEAD"])
        .with_context(|| format!("no common ancestor with {BASE}: fetch it first"))?;
    if base == head {
        return Ok(None);
    }
    let path = record_path(root, &tree)?;
    // A kept review answers only for the change it read: the same tree on
    // another base is another diff.
    if let Some(kept) = read_record(&path)?.filter(|kept| kept.base == base) {
        return Ok(Some(kept));
    }
    let range = format!("{base}..{head}");
    let trailers = git(
        root,
        &[
            "log",
            "--format=%(trailers:key=Co-Authored-By,valueonly)",
            &range,
        ],
    )?;
    let authors = authors(&trailers);
    let Some((agent, independent)) = choose(&authors, installed) else {
        bail!("no reviewer is installed: neither `codex` nor `claude` is on PATH");
    };
    let reply = reviewer(agent, &prompt(&base, &head), root)?;
    let verdict = parse_verdict(&reply)?;
    if git(root, &["rev-parse", "HEAD^{tree}"])? != tree
        || !git(root, &["status", "--porcelain"])?.is_empty()
    {
        bail!("the worktree changed while {agent} reviewed it: nothing was recorded");
    }
    let record = Record {
        schema: SCHEMA.to_string(),
        tree,
        head,
        base,
        level: verdict.level,
        findings: verdict.findings,
        reviewer: agent,
        authors: authors.into_iter().collect(),
        independent,
        at: now(),
        overruled: None,
    };
    write_record(&path, &record)?;
    Ok(Some(record))
}

/// A person lets a blocking review of this commit's tree cross.
pub fn overrule(root: &Path, why: &str) -> Result<Record> {
    let tree = git(root, &["rev-parse", "HEAD^{tree}"])?;
    let path = record_path(root, &tree)?;
    let base = git(root, &["merge-base", BASE, "HEAD"])?;
    let Some(mut record) = read_record(&path)?.filter(|kept| kept.base == base) else {
        bail!("no review is kept for this commit: run `sykli run review.json` first");
    };
    if record.level != Level::Blocking {
        bail!(
            "the review of this commit is `{}`: nothing to overrule",
            record.level
        );
    }
    record.overruled = Some(Overruled {
        why: why.to_string(),
        at: now(),
    });
    write_record(&path, &record)?;
    Ok(record)
}

/// What an agent session sets in its environment.
const AGENT_MARKERS: [&str; 12] = [
    "TOIMIJA_SESSION",
    "TOIMIJA_RUN",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "PI_SESSION_ID",
    "PI_CODING_AGENT",
    "OPENCODE",
    "OPENCODE_SESSION_ID",
    "AI_AGENT",
];

/// Only a person at a terminal overrules: an agent session is refused.
pub fn person_at_terminal(env: &dyn Fn(&str) -> bool, terminal: bool) -> Result<()> {
    if let Some(marker) = AGENT_MARKERS.iter().find(|name| env(name)) {
        bail!("only a person overrules a review: this is an agent session ({marker} is set)");
    }
    if !terminal {
        bail!("only a person overrules a review: run this at a terminal");
    }
    Ok(())
}

pub fn summary(record: &Record) -> String {
    let count = |level| record.findings.iter().filter(|f| f.level == level).count();
    let mut line = format!(
        "review: {} ({} blocking, {} to look at) by {}",
        record.level,
        count(Level::Blocking),
        count(Level::Look),
        record.reviewer
    );
    line.push_str(match record.independent {
        Independent::Yes => ", another agent than the author",
        Independent::No => ", the same agent as the author in a fresh session",
        Independent::Unknown => ", author agent unknown",
    });
    if let Some(overruled) = &record.overruled {
        line.push_str(&format!("; overruled by a person: {}", overruled.why));
    }
    line
}

fn record_path(root: &Path, tree: &str) -> Result<PathBuf> {
    let common = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Ok(Path::new(&common)
        .join("toimija/reviews")
        .join(format!("{tree}.json")))
}

fn read_record(path: &Path) -> Result<Option<Record>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(anyhow!("{}: {error}", path.display())),
    };
    let record: Record =
        serde_json::from_str(&text).with_context(|| format!("{}: not a review", path.display()))?;
    if record.schema != SCHEMA {
        bail!(
            "{}: schema `{}` is not {SCHEMA}",
            path.display(),
            record.schema
        );
    }
    Ok(Some(record))
}

fn write_record(path: &Path, record: &Record) -> Result<()> {
    let dir = path.parent().context("a review record has a directory")?;
    fs::create_dir_all(dir)?;
    let partial = path.with_extension(format!("json.tmp-{}", std::process::id()));
    fs::write(&partial, serde_json::to_string_pretty(record)? + "\n")?;
    fs::rename(&partial, path).with_context(|| format!("{}", path.display()))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").current_dir(root).args(args).output()?;
    if !output.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The agents on PATH, in the order reviewers are tried.
pub fn installed() -> Vec<Agent> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    Agent::ALL
        .into_iter()
        .filter(|agent| std::env::split_paths(&path).any(|dir| dir.join(agent.program()).is_file()))
        .collect()
}

/// Starts the agent with read-only tools, gives it the prompt on stdin and
/// returns what it said last.
pub fn run_agent(agent: Agent, prompt: &str, root: &Path) -> Result<String> {
    let last = std::env::temp_dir().join(format!("toimija-review-{}.txt", std::process::id()));
    let mut child = reviewer_command(agent, &last)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not start {agent}"))?;
    child
        .stdin
        .take()
        .context("the reviewer's stdin is piped")?
        .write_all(prompt.as_bytes())?;
    let reader = drain(
        child
            .stdout
            .take()
            .context("the reviewer's stdout is piped")?,
    );
    let said = drain(
        child
            .stderr
            .take()
            .context("the reviewer's stderr is piped")?,
    );
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > TIME_LIMIT {
            child.kill()?;
            child.wait()?;
            bail!(
                "{agent} did not finish its review in {} minutes",
                TIME_LIMIT.as_secs() / 60
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let printed = reader
        .join()
        .map_err(|_| anyhow!("reading {agent}'s reply panicked"))??;
    let said = said
        .join()
        .map_err(|_| anyhow!("reading {agent}'s messages panicked"))??;
    if !status.success() {
        // What the reviewer said while working is shown only when it failed.
        eprint!("{said}");
        bail!("{agent} ended with {status} before giving a review");
    }
    match agent {
        Agent::Claude => Ok(printed),
        Agent::Codex => {
            let reply = fs::read_to_string(&last)
                .with_context(|| format!("{agent} wrote no last message"))?;
            // The file is ours; a failure to remove it changes nothing.
            let _ = fs::remove_file(&last);
            Ok(reply)
        }
    }
}

fn drain(mut from: impl Read + Send + 'static) -> std::thread::JoinHandle<std::io::Result<String>> {
    std::thread::spawn(move || {
        let mut text = String::new();
        from.read_to_string(&mut text).map(|_| text)
    })
}

/// The reviewer's command line. It starts as a session of its own: no agent
/// marker of the session that asked for the review reaches it.
fn reviewer_command(agent: Agent, last: &Path) -> Command {
    let mut command = Command::new(agent.program());
    for marker in AGENT_MARKERS {
        command.env_remove(marker);
    }
    match agent {
        Agent::Codex => {
            command
                .args(["exec", "--sandbox", "read-only", "--ephemeral", "-o"])
                .arg(last)
                .arg("-");
        }
        Agent::Claude => {
            command.args([
                "-p",
                "--tools",
                "Read,Grep,Glob,Bash",
                "--allowedTools",
                "Read",
                "Grep",
                "Glob",
                "Bash(git diff:*)",
                "Bash(git log:*)",
                "Bash(git show:*)",
            ]);
        }
    }
    command
}

/// `cargo xtask review [--overrule <why>]`, returning the exit code.
pub fn main(root: &Path, args: &[String]) -> u8 {
    let outcome = match args {
        [] => review(root, &installed(), &run_agent).map(|record| match record {
            Some(record) => {
                eprintln!("{}", summary(&record));
                for finding in &record.findings {
                    eprintln!("  [{}] {}", finding.level, finding.text);
                }
                if exit_code(&record) == 1 {
                    eprintln!(
                        "review: blocking stops the push. Fix it and commit, or a person \
                         runs `cargo xtask review --overrule \"<why>\"` at a terminal."
                    );
                }
                exit_code(&record)
            }
            None => {
                eprintln!("review: nothing to review: this commit is already in {BASE}");
                0
            }
        }),
        [flag, why] if flag == "--overrule" && !why.trim().is_empty() => person_at_terminal(
            &|name| std::env::var_os(name).is_some(),
            std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        )
        .and_then(|()| overrule(root, why))
        .map(|record| {
            eprintln!("{}", summary(&record));
            0
        }),
        _ => Err(anyhow!("usage: cargo xtask review [--overrule \"<why>\"]")),
    };
    outcome.unwrap_or_else(|error| {
        eprintln!("review: no review happened: {error:#}");
        2
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn a_reply_without_a_verdict_line_is_no_review() {
        let error = parse_verdict("Looks good to me overall.\n").unwrap_err();
        assert!(error.to_string().contains("no `REVIEW:`"), "{error}");
        assert!(parse_verdict("").is_err());
    }

    #[test]
    fn a_verdict_that_is_not_one_of_the_three_is_no_review() {
        let error = parse_verdict("REVIEW: approved\n").unwrap_err();
        assert!(error.to_string().contains("approved"), "{error}");
    }

    #[test]
    fn the_last_verdict_line_counts_and_findings_are_kept() {
        let verdict = parse_verdict(
            "I first thought REVIEW: blocking but checked.\n\
             - [look] src/cli.rs:10: the help text names a removed flag.\n\
             REVIEW: look\n",
        )
        .unwrap();
        assert_eq!(verdict.level, Level::Look);
        assert_eq!(verdict.findings.len(), 1);
        assert_eq!(
            verdict.findings[0].text,
            "src/cli.rs:10: the help text names a removed flag."
        );
    }

    #[test]
    fn a_verdict_milder_than_its_worst_finding_is_raised() {
        let verdict =
            parse_verdict("- [blocking] src/run.rs:3: drops the exit code.\nREVIEW: fine\n")
                .unwrap();
        assert_eq!(verdict.level, Level::Blocking);
    }

    #[test]
    fn markdown_around_the_verdict_word_is_read() {
        assert_eq!(
            parse_verdict("REVIEW: **Fine**").unwrap().level,
            Level::Fine
        );
    }

    #[test]
    fn the_reviewer_is_another_agent_than_the_author_when_one_is_installed() {
        let claude = authors("Claude Fable 5.2 <noreply@anthropic.com>\n");
        assert_eq!(
            choose(&claude, &[Agent::Codex, Agent::Claude]),
            Some((Agent::Codex, Independent::Yes))
        );
        assert_eq!(
            choose(&claude, &[Agent::Claude]),
            Some((Agent::Claude, Independent::No))
        );
        assert_eq!(
            choose(&authors(""), &[Agent::Codex, Agent::Claude]),
            Some((Agent::Codex, Independent::Unknown))
        );
        assert_eq!(choose(&claude, &[]), None);
    }

    #[test]
    fn an_agent_or_a_pipe_cannot_overrule() {
        let error = person_at_terminal(&|name| name == "CLAUDECODE", true).unwrap_err();
        assert!(error.to_string().contains("CLAUDECODE"), "{error}");
        assert!(person_at_terminal(&|_| false, false).is_err());
        assert!(person_at_terminal(&|_| false, true).is_ok());
    }

    #[test]
    fn a_blocking_review_fails_until_a_person_overrules_it() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = branch_repo("review-blocking");
        let calls = Cell::new(0);
        let blocking = |_: Agent, _: &str, _: &Path| {
            calls.set(calls.get() + 1);
            Ok("- [blocking] new.txt:1: wrong.\nREVIEW: blocking\n".to_string())
        };

        let record = review(&root, &[Agent::Codex], &blocking).unwrap().unwrap();
        assert_eq!(record.level, Level::Blocking);
        assert_eq!(exit_code(&record), 1);

        // The same tree is not reviewed twice: the kept record answers.
        let again = review(&root, &[Agent::Codex], &blocking).unwrap().unwrap();
        assert_eq!(again, record);
        assert_eq!(calls.get(), 1);

        let overruled = overrule(&root, "known and accepted").unwrap();
        assert_eq!(exit_code(&overruled), 0);
        let kept = review(&root, &[Agent::Codex], &blocking).unwrap().unwrap();
        assert_eq!(exit_code(&kept), 0);
        assert_eq!(kept.overruled.unwrap().why, "known and accepted");
    }

    #[test]
    fn a_new_commit_is_reviewed_again() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = branch_repo("review-again");
        let calls = Cell::new(0);
        let fine = |_: Agent, _: &str, _: &Path| {
            calls.set(calls.get() + 1);
            Ok("REVIEW: fine\n".to_string())
        };
        review(&root, &[Agent::Claude], &fine).unwrap().unwrap();
        fs::write(root.join("new.txt"), "changed\n").unwrap();
        run_git(&root, &["commit", "-am", "second"]);
        let second = review(&root, &[Agent::Claude], &fine).unwrap().unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(second.level, Level::Fine);
        assert!(
            overrule(&root, "why").is_err(),
            "fine has nothing to overrule"
        );
    }

    #[test]
    fn uncommitted_work_a_missing_reviewer_and_no_verdict_are_no_review() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = branch_repo("review-none");
        let silent = |_: Agent, _: &str, _: &Path| Ok("I could not decide.\n".to_string());

        let error = review(&root, &[], &silent).unwrap_err();
        assert!(error.to_string().contains("no reviewer"), "{error}");
        let error = review(&root, &[Agent::Codex], &silent).unwrap_err();
        assert!(error.to_string().contains("no `REVIEW:`"), "{error}");
        // Neither left a record that a later run would take for a review.
        let fine = |_: Agent, _: &str, _: &Path| Ok("REVIEW: fine\n".to_string());
        fs::write(root.join("stray.txt"), "x\n").unwrap();
        let error = review(&root, &[Agent::Codex], &fine).unwrap_err();
        assert!(error.to_string().contains("commit first"), "{error}");
    }

    #[test]
    fn a_commit_already_in_the_base_has_nothing_to_review() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = branch_repo("review-landed");
        run_git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let never = |_: Agent, _: &str, _: &Path| -> Result<String> {
            panic!("nothing to review, so no reviewer starts")
        };
        assert_eq!(review(&root, &[Agent::Codex], &never).unwrap(), None);
    }

    /// A repository whose `origin/main` is its first commit, with one commit
    /// on top of it.
    fn branch_repo(name: &str) -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{name}-{id}"));
        fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init", "-q"]);
        run_git(&root, &["config", "user.email", "toimija@example.invalid"]);
        run_git(&root, &["config", "user.name", "Toimija Test"]);
        fs::write(root.join("README.md"), "# Demo\n").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "-q", "-m", "initial"]);
        run_git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        fs::write(root.join("new.txt"), "new\n").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "-q", "-m", "change"]);
        root
    }

    fn run_git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn the_same_tree_on_another_base_is_reviewed_again() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = branch_repo("review-rebased");
        fs::write(root.join("more.txt"), "more\n").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "-q", "-m", "more"]);
        let calls = Cell::new(0);
        let blocking = |_: Agent, _: &str, _: &Path| {
            calls.set(calls.get() + 1);
            Ok("REVIEW: blocking\n".to_string())
        };
        review(&root, &[Agent::Codex], &blocking).unwrap().unwrap();
        overrule(&root, "accepted").unwrap();

        // The base moves under the same tree: the change is now another diff.
        run_git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
        let again = review(&root, &[Agent::Codex], &blocking).unwrap().unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(
            again.overruled, None,
            "an overrule does not follow the tree"
        );
        assert_eq!(exit_code(&again), 1);
    }

    #[test]
    fn the_reviewer_starts_without_the_asking_sessions_agent_markers() {
        for agent in Agent::ALL {
            let command = reviewer_command(agent, Path::new("/tmp/last.txt"));
            let removed: Vec<_> = command
                .get_envs()
                .filter(|(_, value)| value.is_none())
                .map(|(name, _)| name.to_string_lossy().into_owned())
                .collect();
            assert!(removed.contains(&"CLAUDECODE".to_string()), "{removed:?}");
            assert!(
                removed.contains(&"TOIMIJA_SESSION".to_string()),
                "{removed:?}"
            );
        }
    }
}
