//! Finished worktrees: linked worktrees of this repository whose work is done
//! here, so their folders can go.
//!
//! A worktree is finished when all of these hold:
//! - it is a linked worktree, not the main one and not the one the command
//!   runs in (no one removes the folder they stand in);
//! - its branch's upstream is gone on origin (deleted there and pruned here,
//!   Git's `[gone]`), or its branch was pushed to an upstream of its own that
//!   is still there and the remote default branch already has its HEAD (a
//!   merge that kept the branch). A branch that still tracks the default
//!   branch or has no upstream, and a detached HEAD, are never listed: by
//!   Git's state they cannot be told from a worktree made a minute ago;
//! - nothing in it is the repository's work left uncommitted, as the packet
//!   counts it: tool state under `.toimija/` and `.sykli/` is not work;
//! - it is not locked, and its folder is there;
//! - no session that is not provably over has a marker in it.
//!
//! "Branch gone on origin" is what Git shows, never "merged": a deleted branch
//! is no proof of a merge. Toimija lists finished worktrees with the command
//! that removes each, and removes nothing.
//!
//! Whether there is any other worktree at all is read from the files in the
//! common git dir, so a repository with none pays no git process. With some:
//! one `git worktree list`, one `for-each-ref` for the branches' upstreams and
//! the remote default branch, one `rev-list` for which HEADs that branch
//! lacks, and one `git status` per worktree that could be finished.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::command::CommandRunner;
use crate::coverage::{Coverage, CoverageStatus};
use crate::git::{self, Worktree};

/// The coverage source label for the finished-worktree listing.
const SOURCE: &str = "finished worktrees (git worktree list, for-each-ref, rev-list, status)";

/// A linked worktree whose work is done here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishedWorktree {
    #[serde(with = "crate::path_codec::one")]
    pub path: PathBuf,
    /// Its branch's short name. A detached HEAD is never listed.
    pub branch: String,
    pub reason: Reason,
    /// The upstream that is gone, or the remote default branch that already
    /// has HEAD, by its short name (`origin/main`).
    pub against: String,
}

/// Why a worktree is finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Its branch's upstream is gone on origin.
    BranchGone,
    /// The remote default branch already has its HEAD.
    InBase,
}

/// The finished worktrees, and how completely they were looked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub worktrees: Vec<FinishedWorktree>,
    pub coverage: Coverage,
}

impl Finished {
    fn none() -> Self {
        Self {
            worktrees: Vec::new(),
            coverage: Coverage::available(SOURCE),
        }
    }

    fn failed(reason: impl Into<String>) -> Self {
        Self {
            worktrees: Vec::new(),
            coverage: Coverage::failed(SOURCE, reason),
        }
    }

    /// A warning for a listing that is not whole, so a short listing is never
    /// read as the whole of it.
    pub fn warning(&self) -> Option<String> {
        (self.coverage.status != CoverageStatus::Available).then(|| {
            format!(
                "finished worktrees not all worked out: {}",
                self.coverage.reason.as_deref().unwrap_or("unknown")
            )
        })
    }
}

/// The finished worktrees of the repository `here` is a worktree of, sorted
/// by path. Read-only, no network.
pub fn find<R: CommandRunner>(runner: &R, here: &Worktree) -> Finished {
    let Some(common_dir) = here.common_dir.as_deref() else {
        return Finished::failed("the repository's common git dir is not known");
    };
    match others_on_disk(common_dir, here.absolute_git_dir.as_deref()) {
        Ok(false) => return Finished::none(),
        Ok(true) => {}
        Err(error) => {
            return Finished::failed(format!(
                "the worktrees folder of the common git dir is unreadable: {}",
                error.kind()
            ));
        }
    }
    let listing = match git::run_git_observed(
        runner,
        &here.root,
        &["worktree", "list", "--porcelain", "-z"],
    ) {
        Ok(bytes) => parse_worktree_list(&bytes),
        Err(reason) => return Finished::failed(format!("git worktree list: {reason}")),
    };
    let current = canonical(&here.root);
    let linked: Vec<Listed> = listing
        .into_iter()
        .skip(1)
        .filter(|entry| !entry.locked && !entry.prunable && !entry.bare)
        .filter(|entry| entry.head.is_some() && canonical(&entry.path) != current)
        .collect();
    if linked.is_empty() {
        return Finished::none();
    }
    let refs = match git::run_git_observed(runner, &here.root, &FOR_EACH_REF) {
        Ok(bytes) => RefListing::parse(&String::from_utf8_lossy(&bytes)),
        Err(reason) => return Finished::failed(format!("git for-each-ref: {reason}")),
    };

    // Gone on origin first. A branch pushed to an upstream of its own that is
    // still there is finished if the remote default branch already has its
    // HEAD, asked for all such at once. A branch still tracking the default
    // branch, one with no upstream, and a detached HEAD are never listed: by
    // Git's state they cannot be told from a worktree made a minute ago for
    // work about to start.
    let base = refs.base();
    let mut candidates = Vec::new();
    // Branches with an upstream of their own still there, with their HEADs.
    let mut unsettled: Vec<(FinishedWorktree, String)> = Vec::new();
    for entry in linked {
        let (Some(branch), Some(head)) = (entry.branch, entry.head) else {
            continue;
        };
        let mut candidate = FinishedWorktree {
            path: entry.path,
            branch: short_ref(&branch).to_owned(),
            reason: Reason::BranchGone,
            against: String::new(),
        };
        if let Some(upstream) = refs.gone_upstream(&branch) {
            candidate.against = upstream;
            candidates.push(candidate);
        } else if base
            .as_deref()
            .is_some_and(|base| refs.has_own_upstream(&branch, base))
        {
            unsettled.push((candidate, head));
        }
    }
    if let (Some(base), false) = (base, unsettled.is_empty()) {
        let mut args = vec!["rev-list"];
        args.extend(unsettled.iter().map(|(_, head)| head.as_str()));
        args.extend(["--not", base.as_str()]);
        let lacking: BTreeSet<String> = match git::run_git_observed(runner, &here.root, &args) {
            Ok(bytes) => String::from_utf8_lossy(&bytes)
                .lines()
                .map(str::to_owned)
                .collect(),
            Err(reason) => return Finished::failed(format!("git rev-list: {reason}")),
        };
        for (mut candidate, head) in unsettled {
            if !lacking.contains(&head) {
                candidate.reason = Reason::InBase;
                candidate.against = short_ref(&base).to_owned();
                candidates.push(candidate);
            }
        }
    }

    let mut finished = Finished::none();
    for candidate in candidates {
        match settled(runner, &candidate.path) {
            Ok(true) => finished.worktrees.push(candidate),
            Ok(false) => {}
            Err(reason) => {
                debug!(
                    event = "toimija.worktrees.unreadable",
                    path = %candidate.path.display(),
                    reason = %reason
                );
                finished.coverage = Coverage::degraded(
                    SOURCE,
                    format!("{}: {reason}", crate::path_codec::display(&candidate.path)),
                );
            }
        }
    }
    finished.worktrees.sort_by(|a, b| a.path.cmp(&b.path));
    finished
}

/// Whether nothing in the worktree at `path` is left to do: no session there
/// that is not provably over (an unreadable marker counts as one), and none
/// of the repository's work uncommitted. The sessions are read first, from
/// files, so a busy worktree costs no `git status`.
fn settled<R: CommandRunner>(runner: &R, path: &Path) -> Result<bool, String> {
    let sessions = crate::sessions::report(runner, path)
        .map_err(|error| format!("its session markers are unreadable: {error:#}"))?;
    if !sessions.sessions.is_empty() || sessions.coverage.status != CoverageStatus::Available {
        return Ok(false);
    }
    let status = git::read_parsed_status(runner, path)
        .map_err(|error| format!("git status failed: {error}"))?;
    Ok(!git::is_dirty(&status))
}

/// Whether the common git dir names a linked worktree other than the one at
/// `own_git_dir`: an entry in `<common>/worktrees/`, read from files.
fn others_on_disk(common_dir: &Path, own_git_dir: Option<&Path>) -> std::io::Result<bool> {
    let entries = match std::fs::read_dir(common_dir.join("worktrees")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let own = own_git_dir.map(canonical);
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() && Some(canonical(&entry.path())) != own {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The path with symlinks resolved, or as given where it cannot be (a folder
/// that is gone), so two spellings of one folder compare equal.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// One worktree as `git worktree list --porcelain -z` names it.
#[derive(Debug, Default, PartialEq, Eq)]
struct Listed {
    path: PathBuf,
    head: Option<String>,
    /// The full ref (`refs/heads/x`); `None` when detached.
    branch: Option<String>,
    bare: bool,
    locked: bool,
    prunable: bool,
}

/// Parse `git worktree list --porcelain -z`: one NUL-terminated attribute per
/// field, and an empty one after each worktree. The main worktree is first.
fn parse_worktree_list(bytes: &[u8]) -> Vec<Listed> {
    let mut listed = Vec::new();
    let mut current: Option<Listed> = None;
    for field in bytes.split(|byte| *byte == 0) {
        if field.is_empty() {
            listed.extend(current.take());
            continue;
        }
        let (key, value) = match field.iter().position(|byte| *byte == b' ') {
            Some(at) => (&field[..at], &field[at + 1..]),
            None => (field, &[][..]),
        };
        if key == b"worktree" {
            listed.extend(current.take());
            current = Some(Listed {
                path: crate::path_codec::from_git_bytes(value),
                ..Listed::default()
            });
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        match key {
            b"HEAD" => entry.head = Some(String::from_utf8_lossy(value).into_owned()),
            b"branch" => entry.branch = Some(String::from_utf8_lossy(value).into_owned()),
            b"bare" => entry.bare = true,
            b"locked" => entry.locked = true,
            b"prunable" => entry.prunable = true,
            _ => {}
        }
    }
    listed.extend(current);
    listed
}

/// Every local branch with its upstream and whether that is gone, and the
/// remote heads the default branch is found from, in one process.
const FOR_EACH_REF: [&str; 6] = [
    "for-each-ref",
    "--format=%(refname)%00%(upstream)%00%(upstream:track)%00%(symref)",
    "refs/heads",
    "refs/remotes/*/HEAD",
    "refs/remotes/*/main",
    "refs/remotes/*/master",
];

/// What [`FOR_EACH_REF`] printed.
#[derive(Debug, Default)]
struct RefListing {
    /// `(refname, upstream, track, symref)` per ref.
    rows: Vec<(String, String, String, String)>,
}

impl RefListing {
    fn parse(text: &str) -> Self {
        let rows = text
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\0');
                Some((
                    fields.next()?.to_owned(),
                    fields.next()?.to_owned(),
                    fields.next()?.to_owned(),
                    fields.next()?.to_owned(),
                ))
            })
            .collect();
        Self { rows }
    }

    /// The short name of `branch`'s upstream when Git reports it gone.
    fn gone_upstream(&self, branch: &str) -> Option<String> {
        self.rows
            .iter()
            .find(|(refname, upstream, track, _)| {
                refname == branch && !upstream.is_empty() && track == "[gone]"
            })
            .map(|(_, upstream, _, _)| short_ref(upstream).to_owned())
    }

    /// Whether `branch` was pushed to an upstream of its own that Git does not
    /// report gone: one that is not `base`, the remote default branch.
    fn has_own_upstream(&self, branch: &str, base: &str) -> bool {
        self.rows.iter().any(|(refname, upstream, track, _)| {
            refname == branch && !upstream.is_empty() && upstream != base && track != "[gone]"
        })
    }

    /// The remote default branch, as the packet's base is found:
    /// `origin/HEAD`'s target, else what [`git::remote_default_branch`] picks.
    /// A full ref (`refs/remotes/origin/main`); `None` with no remote one.
    fn base(&self) -> Option<String> {
        let remotes = || {
            self.rows
                .iter()
                .filter(|(refname, ..)| refname.starts_with("refs/remotes/"))
        };
        if let Some((_, _, _, target)) = remotes().find(|(refname, _, _, target)| {
            refname == "refs/remotes/origin/HEAD" && target.starts_with("refs/remotes/")
        }) {
            return Some(target.clone());
        }
        let listing: String = remotes()
            .map(|(refname, _, _, target)| format!("{refname} {target}\n"))
            .collect();
        git::remote_default_branch(&listing).map(|short| format!("refs/remotes/{short}"))
    }
}

/// `refs/heads/x` → `x`, `refs/remotes/origin/x` → `origin/x`.
fn short_ref(full: &str) -> &str {
    full.strip_prefix("refs/heads/")
        .or_else(|| full.strip_prefix("refs/remotes/"))
        .unwrap_or(full)
}

/// The `sessions` section: each finished worktree, why, and the command that
/// removes it, then what to check first. Empty when there are none.
pub fn render_text(worktrees: &[FinishedWorktree]) -> String {
    let mut out = String::new();
    if worktrees.is_empty() {
        return out;
    }
    out.push_str("FINISHED WORKTREES (nothing uncommitted, no live session)\n");
    for worktree in worktrees {
        let path = crate::path_codec::display(&worktree.path);
        let why = match worktree.reason {
            Reason::BranchGone => "branch gone on origin".to_string(),
            Reason::InBase => format!("nothing {} lacks", worktree.against),
        };
        let name = &worktree.branch;
        writeln!(out, "  {path}  ({name}: {why})").unwrap();
        writeln!(out, "    git worktree remove {}", shell_word(&path)).unwrap();
    }
    out.push_str(
        "  Check that no terminal is inside one before removing it. Its branch stays; \
         ignored files such as build output go with the folder.\n",
    );
    out
}

/// The packet's one line, when there is any finished worktree.
pub fn packet_line(worktrees: &[FinishedWorktree]) -> Option<String> {
    (!worktrees.is_empty()).then(|| {
        format!(
            "- finished worktrees: {} elsewhere (`toimija sessions` lists them)",
            worktrees.len()
        )
    })
}

/// `text` as one shell word: as it is when it needs no quoting, else in
/// single quotes.
fn shell_word(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+@:,=%".contains(c));
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_porcelain_listing_names_each_worktree_and_its_state() {
        let bytes = b"worktree /r\0HEAD aaa\0branch refs/heads/main\0\0\
            worktree /w/a b\0HEAD bbb\0detached\0\0\
            worktree /w/c\0HEAD ccc\0branch refs/heads/c\0locked\0\0\
            worktree /w/d\0HEAD ddd\0branch refs/heads/d\0prunable gitdir file points to non-existent location\0\0";
        let listed = parse_worktree_list(bytes);
        assert_eq!(listed.len(), 4);
        assert_eq!(listed[0].branch.as_deref(), Some("refs/heads/main"));
        assert_eq!(listed[1].path, PathBuf::from("/w/a b"));
        assert_eq!(listed[1].head.as_deref(), Some("bbb"));
        assert_eq!(listed[1].branch, None);
        assert!(listed[2].locked && !listed[2].prunable);
        assert!(listed[3].prunable && !listed[3].locked);
    }

    #[test]
    fn only_a_gone_upstream_counts_as_gone() {
        let refs = RefListing::parse(
            "refs/heads/a\0refs/remotes/origin/a\0[gone]\0\n\
             refs/heads/b\0refs/remotes/origin/b\0[ahead 1]\0\n\
             refs/heads/c\0\0\0\n\
             refs/heads/d\0refs/remotes/origin/main\0\0\n",
        );
        assert_eq!(
            refs.gone_upstream("refs/heads/a").as_deref(),
            Some("origin/a")
        );
        for branch in [
            "refs/heads/b",
            "refs/heads/c",
            "refs/heads/d",
            "refs/heads/x",
        ] {
            assert_eq!(refs.gone_upstream(branch), None, "{branch}");
        }
        // Only `b` has an upstream of its own still there: `a`'s is gone,
        // `c` has none, and `d` tracks the default branch itself.
        let base = "refs/remotes/origin/main";
        let own: Vec<&str> = [
            "refs/heads/a",
            "refs/heads/b",
            "refs/heads/c",
            "refs/heads/d",
        ]
        .into_iter()
        .filter(|branch| refs.has_own_upstream(branch, base))
        .collect();
        assert_eq!(own, ["refs/heads/b"]);
    }

    #[test]
    fn the_base_is_origin_head_then_the_remote_default_branch() {
        let with_head = RefListing::parse(
            "refs/remotes/origin/HEAD\0\0\0refs/remotes/origin/trunk\n\
             refs/remotes/origin/main\0\0\0\n",
        );
        assert_eq!(
            with_head.base().as_deref(),
            Some("refs/remotes/origin/trunk")
        );
        let without = RefListing::parse("refs/remotes/origin/master\0\0\0\n");
        assert_eq!(
            without.base().as_deref(),
            Some("refs/remotes/origin/master")
        );
        // A local branch is never the base: it may be behind.
        assert_eq!(RefListing::parse("refs/heads/main\0\0\0\n").base(), None);
    }

    #[test]
    fn a_path_is_quoted_only_when_the_shell_would_split_or_expand_it() {
        assert_eq!(shell_word("/w/plain-name_1.2"), "/w/plain-name_1.2");
        assert_eq!(shell_word("/w/a b"), "'/w/a b'");
        assert_eq!(shell_word("/w/it's"), r"'/w/it'\''s'");
        assert_eq!(shell_word("/w/$HOME"), "'/w/$HOME'");
    }

    #[test]
    fn the_packet_line_is_there_only_when_there_is_something_to_remove() {
        assert_eq!(packet_line(&[]), None);
        let one = FinishedWorktree {
            path: PathBuf::from("/w/a"),
            branch: "docs/setup".into(),
            reason: Reason::InBase,
            against: "origin/main".into(),
        };
        assert_eq!(
            packet_line(&[one]).as_deref(),
            Some("- finished worktrees: 1 elsewhere (`toimija sessions` lists them)")
        );
    }
}
