//! `toimija watch` — keep the live packet fresh while you work.
//!
//! A foreground command (NOT a daemon): it watches the repo for filesystem and
//! Git-state changes and re-runs a [`refresh::Plan`](crate::refresh::Plan) after
//! a debounce window, until it is stopped (Ctrl-C on the process, or the `stop`
//! flag that `run` drives). No network, no validation execution. Toimija's own
//! live output is ignored, so writing the live packet — `.toimija/current.*`,
//! or a per-session `.toimija/sessions/<id>/current.*` under `run` — never
//! re-triggers a refresh.
//!
//! # What a tick costs
//!
//! The repository root, git dir and common dir come from the command's one
//! discovery (`Repository::of`) and are passed down; nothing is discovered
//! again per tick. A refresh reads one `git status` and, while the refs' stamp
//! holds, carries the last read's ref-derived facts (`Carried`). When nothing
//! changes, a tick starts no git process:
//!
//! - **Work-tree changes** arrive as filesystem events — FSEvents on macOS,
//!   inotify on Linux — and refresh after a debounce. Read-only access events
//!   are dropped, so a refresh's own reads never re-trigger it. Events under a
//!   build directory at the root (`target/`, `_build/`, `deps/`,
//!   `node_modules/`) prompt one `git status` instead of a refresh: untracked
//!   build output changes nothing, while a tracked file there shows as
//!   modified. A tracked path is never ignored.
//! - **Git's own state** is a `Stamp`: the metadata of HEAD, the index, the
//!   operation markers, the refs (files or reftable, the worktree's own too),
//!   packed-refs and config, read with `stat` alone. A moved ref or config
//!   refreshes. A moved HEAD or index is checked first with one `git status`,
//!   because other tools rewrite the index without changing anything a packet
//!   shows. Events inside `.git/` only prompt this check.
//! - **Without events** — when inotify cannot arm (its watch limit, say) —
//!   Linux falls back to Git polling: one `git status` per tick.
//! - **A safety net** runs once every `SAFETY_NET_INTERVAL` (a minute): one
//!   `git status` compared with the packet, together with the changed files'
//!   contents — the packet's consistency token. What no event and no stamp
//!   reported (a dropped event, a change to a global excludes file) is caught
//!   there.
//! - **A failed refresh** is retried after half a second, then after a delay
//!   that doubles up to the safety net's minute, and at once on a new change.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use notify::{Config, Event, EventKind, RecursiveMode, Watcher};
use tracing::{debug, info, warn};

use crate::command::CommandRunner;
use crate::consistency::{self, Consistency};
use crate::context::ContextPacket;
use crate::git::{self, GitState};
use crate::refresh;

/// Coalesce a burst of events for this long before refreshing once.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the wait loop wakes to re-check the stop flag.
const TICK: Duration = Duration::from_millis(200);
/// A wall/monotonic disagreement this large is suspension, not scheduler jitter.
const SUSPENSION_THRESHOLD: Duration = Duration::from_secs(30);
/// How long a watcher has to report its readiness probe before fallback/failure.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the Git state is checked: the [`Stamp`], and under Git polling one
/// `git status`.
const STATE_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// How often the safety net compares one `git status`, and the changed files'
/// contents, with the packet, whatever the events and stamps said.
const SAFETY_NET_INTERVAL: Duration = Duration::from_secs(60);
/// The first retry of a failed refresh; each later one waits twice as long, up
/// to [`SAFETY_NET_INTERVAL`].
const RETRY_FIRST: Duration = Duration::from_millis(500);
/// Build-output directories at the repository root. Their churn is checked with
/// one `git status`, never refreshed on directly, and never ignored outright: a
/// tracked file in one is still source.
const BUILD_DIRS: [&str; 4] = ["target", "_build", "deps", "node_modules"];
/// The readiness probe's file name, in the repository root: see [`prove_ready`].
const PROBE_PREFIX: &str = "toimija-watch-probe-";
/// Markers of an in-progress Git operation, in the git dir. The consistency
/// token reads them.
const OPERATION_MARKERS: [&str; 6] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
    "rebase-merge",
    "rebase-apply",
];

/// Both clocks sampled together. Wall time advances during suspension while
/// the monotonic clock does not on macOS and Linux.
struct ClockPair {
    wall: SystemTime,
    mono: Instant,
}

impl ClockPair {
    fn now() -> Self {
        Self {
            wall: SystemTime::now(),
            mono: Instant::now(),
        }
    }
}

/// Return the time spent suspended when wall time outruns monotonic time by at
/// least `threshold`. Backwards wall-clock adjustments never fire.
fn detect_suspension(prev: &ClockPair, now: &ClockPair, threshold: Duration) -> Option<Duration> {
    let wall_delta = now.wall.duration_since(prev.wall).ok()?;
    let mono_delta = now.mono.checked_duration_since(prev.mono)?;
    let slept = wall_delta.checked_sub(mono_delta)?;
    (slept >= threshold).then_some(slept)
}

/// The loop's cadences, as one value so tests can run the real loop fast.
#[derive(Debug, Clone, Copy)]
struct Timing {
    tick: Duration,
    state_poll: Duration,
    safety_net: Duration,
    retry_first: Duration,
}

impl Timing {
    const LIVE: Timing = Timing {
        tick: TICK,
        state_poll: STATE_POLL_INTERVAL,
        safety_net: SAFETY_NET_INTERVAL,
        retry_first: RETRY_FIRST,
    };
}

/// How long to wait before retrying a failed refresh: the first retry after
/// `retry_first`, each later one twice as long, never longer than the safety
/// net's interval.
fn next_retry_delay(previous: Option<Duration>, timing: Timing) -> Duration {
    previous.map_or(timing.retry_first, |delay| {
        (delay * 2).min(timing.safety_net)
    })
}

/// What a watch finds once and passes down: never discovered again per tick.
#[derive(Debug, Clone)]
pub struct Repository {
    /// What every refresh is handed: the discovery's facts that last as long
    /// as the worktree ([`git::Worktree::lasting`]).
    worktree: git::Worktree,
    /// The work tree's root as Git names it.
    root: PathBuf,
    /// The same root with symlinks resolved: FSEvents reports `/private/tmp/x`
    /// for a repository at `/tmp/x`.
    canonical_root: PathBuf,
    /// This worktree's own git dir: HEAD, the index, operation markers.
    git_dir: PathBuf,
    /// The git dir every worktree shares: refs, packed-refs, config.
    common_dir: PathBuf,
    /// The Git files' metadata when the repository was discovered.
    discovered: Stamp,
}

impl Repository {
    /// The repository of a discovered worktree, and the Git files' stamp. No
    /// git process when the discovery named the git dir and the common dir;
    /// otherwise one ([`Repository::discover`]). Call it before building the
    /// packet the watch starts from: the first check compares against this
    /// stamp, so a change made while that packet was built is not missed.
    pub fn of(runner: &impl CommandRunner, worktree: &git::Worktree) -> Result<Self> {
        let (git_dir, common_dir) = match (&worktree.absolute_git_dir, &worktree.common_dir) {
            (Some(git_dir), Some(common_dir)) => (git_dir.clone(), common_dir.clone()),
            _ => Self::locate(runner, &worktree.root)?,
        };
        Ok(Self::with(worktree.lasting(), git_dir, common_dir))
    }

    /// Find the git dir and the common dir of the worktree at `root`, with one
    /// git process, and stamp the Git files.
    pub fn discover(runner: &impl CommandRunner, root: &Path) -> Result<Self> {
        let (git_dir, common_dir) = Self::locate(runner, root)?;
        Ok(Self::at(root, git_dir, common_dir))
    }

    /// The worktree every refresh of this watch reads.
    pub fn worktree(&self) -> &git::Worktree {
        &self.worktree
    }

    /// Whether the refs, packed-refs, config and `shallow` are as they were
    /// when the repository was discovered: then whatever a collection read
    /// from them since still holds.
    pub fn refs_held(&self) -> bool {
        self.stamp().refs == self.discovered.refs
    }

    /// The git dir and the common dir of the worktree at `root`: one git
    /// process.
    fn locate(runner: &impl CommandRunner, root: &Path) -> Result<(PathBuf, PathBuf)> {
        let args = ["rev-parse", "--absolute-git-dir", "--git-common-dir"];
        let output = runner
            .run(root, "git", &args)
            .context("failed to run git to find the git dir")?;
        anyhow::ensure!(
            output.status == 0,
            "`git {}` failed in {}: {}",
            args.join(" "),
            root.display(),
            output.stderr_lossy().trim()
        );
        let mut lines = output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty());
        let (Some(git_dir), Some(common_dir)) = (lines.next(), lines.next()) else {
            anyhow::bail!("`git {}` named no git dir", args.join(" "));
        };
        let git_dir = crate::path_codec::from_git_bytes(git_dir);
        // Relative to the directory git ran in; `join` keeps an absolute one.
        let common_dir = root.join(crate::path_codec::from_git_bytes(common_dir));
        Ok((git_dir, common_dir))
    }

    fn at(root: &Path, git_dir: PathBuf, common_dir: PathBuf) -> Self {
        Self::with(
            git::Worktree::at_root(root.to_path_buf()),
            git_dir,
            common_dir,
        )
    }

    fn with(worktree: git::Worktree, git_dir: PathBuf, common_dir: PathBuf) -> Self {
        let root = worktree.root.clone();
        let canonical_root = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let worktree = git::Worktree {
            absolute_git_dir: Some(git_dir.clone()),
            common_dir: Some(common_dir.clone()),
            ..worktree
        };
        let mut repository = Self {
            worktree,
            root,
            canonical_root,
            git_dir,
            common_dir,
            discovered: Stamp::default(),
        };
        repository.discovered = repository.stamp();
        repository
    }

    /// Stamp the files whose change can move a packet without a work-tree
    /// event. `stat` only: no git process, no file contents.
    fn stamp(&self) -> Stamp {
        let git_dir = &self.git_dir;
        let common = &self.common_dir;
        let mut status = vec![
            entry(git_dir.join("HEAD")),
            entry(git_dir.join("index")),
            entry(common.join("info").join("exclude")),
        ];
        status.extend(
            OPERATION_MARKERS
                .iter()
                .map(|marker| entry(git_dir.join(marker))),
        );
        let mut refs = vec![
            entry(common.join("packed-refs")),
            entry(common.join("config")),
            entry(git_dir.join("config.worktree")),
            entry(common.join("shallow")),
            // History without any ref moving: a graft rewrites parents, and
            // alternates change which objects the history reads can reach.
            entry(common.join("info").join("grafts")),
            entry(common.join("objects").join("info").join("alternates")),
            entry(self.root.join(crate::config::CONFIG_PATH)),
        ];
        // `refs/toimija` is Toimija's own footprint (snapshot leases).
        let own = common.join("refs").join("toimija");
        walk(&common.join("refs"), &own, &mut refs);
        walk(&common.join("reftable"), &own, &mut refs);
        if git_dir != common {
            // A linked worktree's own refs: bisect, worktree, rewritten — and,
            // in a reftable repository, its HEAD.
            walk(&git_dir.join("refs"), &own, &mut refs);
            walk(&git_dir.join("reftable"), &own, &mut refs);
        }
        Stamp { status, refs }
    }

    /// `path` relative to the repository root, under either spelling of it.
    fn relative<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        path.strip_prefix(&self.root)
            .or_else(|_| path.strip_prefix(&self.canonical_root))
            .ok()
    }

    /// What an event at `path` asks for. A path under neither spelling of the
    /// root is not evidence of a work-tree change, so it only prompts a stamp
    /// check, which starts no git process; it can never make the watch refresh
    /// on its own output. (The readiness probe proved that the backend reports
    /// one of the two spellings.)
    fn classify(&self, path: &Path) -> Change {
        let Some(relative) = self.relative(path) else {
            debug!(event = "toimija.watch.event_outside_root", path = %path.display());
            return Change::Git;
        };
        if is_ignored(relative) {
            Change::Ignored
        } else if first_component(relative) == Some(".git") {
            Change::Git
        } else if is_build_output(relative) {
            Change::Build
        } else {
            Change::Tree
        }
    }

    fn classify_event(&self, event: &Event) -> Change {
        event
            .paths
            .iter()
            .map(|path| self.classify(path))
            .max()
            .unwrap_or(Change::Ignored)
    }

    /// Swallow the rest of an event burst: keep receiving until the channel is
    /// quiet for a full debounce window, so one logical change yields one
    /// refresh. Returns the strongest change the burst carried. Backend errors
    /// inside the burst keep their warning — rescan-required is precisely the
    /// error that means events were lost.
    fn drain_burst(&self, rx: &mpsc::Receiver<notify::Result<Event>>, first: Change) -> Change {
        let mut change = first;
        while let Ok(result) = rx.recv_timeout(DEBOUNCE) {
            match result {
                Ok(event) => change = change.max(self.classify_event(&event)),
                Err(error) => self.warn_watcher_error(&error),
            }
        }
        change
    }

    /// Warn about a backend error unless it is scoped entirely to ignored
    /// directories: a build deleting files under `_build`/`target` mid-scan is
    /// churn the watch has promised not to react to, not a packet-freshness
    /// problem. Polling can also race children that vanish mid-walk; those
    /// path-scoped `NotFound` errors are quiet unless the watched root itself
    /// is gone. A pathless error says nothing about where it happened, so it
    /// stays visible.
    fn warn_watcher_error(&self, error: &notify::Error) {
        if !self.error_is_ignorable(error) {
            eprintln!("toimija: file watcher error (packet may be stale): {error}");
        }
    }

    fn error_is_ignorable(&self, error: &notify::Error) -> bool {
        if error.paths.is_empty() {
            return false;
        }
        if matches!(
            &error.kind,
            notify::ErrorKind::Io(io_error)
                if io_error.kind() == std::io::ErrorKind::NotFound
        ) {
            return self.root.try_exists().unwrap_or(false);
        }
        error.paths.iter().all(|path| {
            self.relative(path)
                .is_some_and(|relative| is_ignored(relative) || is_build_output(relative))
        })
    }
}

/// What a filesystem event asks the watch to do, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Change {
    /// Generated output, build/vendor churn, Toimija's own Git footprint.
    Ignored,
    /// Inside `.git/`: check the [`Stamp`], which decides.
    Git,
    /// A build directory at the root: check the stamp and one `git status`,
    /// which shows a tracked file there as modified.
    Build,
    /// The work tree: refresh.
    Tree,
}

/// The metadata of the Git files that can move a packet without a work-tree
/// event, read with `stat` alone. Git replaces each of these files through a
/// lock file renamed into place, so every write is a new inode and a new
/// change time, never only new bytes under an old stamp.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Stamp {
    /// What `git status` reads: HEAD, the index, the operation markers and
    /// `info/exclude`. A change here is checked with one `git status` before
    /// refreshing.
    status: Vec<Entry>,
    /// What only a refresh reads: refs, packed-refs, config, `shallow`,
    /// `info/grafts`, `objects/info/alternates`, and the packet's own
    /// configuration. A change here refreshes, and reads them again.
    refs: Vec<Entry>,
}

type Entry = (PathBuf, Option<FileStamp>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    inode: u64,
    changed: (i64, i64),
}

fn entry(path: PathBuf) -> Entry {
    let stamp = fs::metadata(&path).ok().map(|meta| file_stamp(&meta));
    (path, stamp)
}

#[cfg(unix)]
fn file_stamp(meta: &fs::Metadata) -> FileStamp {
    use std::os::unix::fs::MetadataExt;
    FileStamp {
        len: meta.len(),
        modified: meta.modified().ok(),
        inode: meta.ino(),
        changed: (meta.ctime(), meta.ctime_nsec()),
    }
}

#[cfg(not(unix))]
fn file_stamp(meta: &fs::Metadata) -> FileStamp {
    FileStamp {
        len: meta.len(),
        modified: meta.modified().ok(),
        inode: 0,
        changed: (0, 0),
    }
}

/// Stamp `dir` and everything under it, in name order, except `skip`.
/// Directories are stamped too: Git writes a lock file beside each ref it
/// updates, so a ref update always changes its directory as well. Symlinks are
/// stamped, never followed.
fn walk(dir: &Path, skip: &Path, out: &mut Vec<Entry>) {
    out.push(entry(dir.to_path_buf()));
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = read
        .filter_map(|child| child.ok().map(|c| c.path()))
        .collect();
    children.sort();
    for child in children {
        if child == skip {
            continue;
        }
        match fs::symlink_metadata(&child) {
            Ok(meta) if meta.is_dir() => walk(&child, skip, out),
            Ok(meta) => out.push((child, Some(file_stamp(&meta)))),
            Err(_) => out.push((child, None)),
        }
    }
}

/// What one `git status` observed, in the form the packet is compared in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Observed {
    status: String,
    /// The consistency token, which also reads the changed files' contents:
    /// computed only for a deep probe.
    consistency: Option<String>,
}

/// What the watch believes the packet was built from: the Git files' stamp
/// from before the last refresh read them, and the packet's status-visible
/// state and consistency token.
struct Known {
    stamp: Stamp,
    status: Option<String>,
    consistency: Option<String>,
    /// The last refresh failed: when to retry it, and the delay that led there.
    retry: Option<Retry>,
    /// A refresh failed with no `git status` behind it: take the next one as
    /// seen rather than as a change, so the same state does not retry it on
    /// every tick. The backoff retries it.
    adopt_next: bool,
}

#[derive(Debug, Clone, Copy)]
struct Retry {
    at: Instant,
    delay: Duration,
}

impl Known {
    fn new(stamp: Stamp) -> Self {
        Self {
            stamp,
            status: None,
            consistency: None,
            retry: None,
            adopt_next: false,
        }
    }

    fn learn(&mut self, packet: &ContextPacket) {
        self.status = Some(status_key(&packet.git));
        self.consistency = Some(packet.consistency.token.clone());
        self.retry = None;
        self.adopt_next = false;
    }

    /// A refresh failed. Schedule its retry on the backoff, and take what
    /// prompted it as seen, so the same evidence does not retry it on every
    /// tick.
    fn fail(&mut self, observed: Option<Observed>, timing: Timing, now: Instant) {
        let delay = next_retry_delay(self.retry.map(|retry| retry.delay), timing);
        self.retry = Some(Retry {
            at: now + delay,
            delay,
        });
        match observed {
            Some(observed) => self.adopt(observed),
            None => self.adopt_next = true,
        }
    }

    fn adopt(&mut self, observed: Observed) {
        self.status = Some(observed.status);
        if let Some(consistency) = observed.consistency {
            self.consistency = Some(consistency);
        }
        self.adopt_next = false;
    }

    fn retry_due(&self, now: Instant) -> bool {
        self.retry.is_some_and(|retry| now >= retry.at)
    }

    fn differs(&self, observed: &Observed) -> bool {
        self.status.as_ref() != Some(&observed.status)
            || observed
                .consistency
                .as_ref()
                .is_some_and(|token| self.consistency.as_ref() != Some(token))
    }
}

/// A packet's Git state reduced to what one `git status` reads — HEAD, the
/// branch, the upstream's name, and the staged, unstaged, untracked and
/// conflicted files — as a token. The ref-derived fields (base, merge-base,
/// ahead/behind, branch diff, recent commits) are left out: the [`Stamp`]
/// watches the refs they come from. The root and git dir are left out too, so
/// two spellings of one path never read as a change.
fn status_key(git: &GitState) -> String {
    let view = GitState {
        repo_root: PathBuf::new(),
        git_dir: None,
        branch: git.branch.clone(),
        head_oid: git.head_oid.clone(),
        upstream: git.upstream.clone(),
        ahead: None,
        behind: None,
        ahead_behind_ref: None,
        base: None,
        merge_base: None,
        detached: git.detached,
        dirty: false,
        staged_files: git.staged_files.clone(),
        unstaged_files: git.unstaged_files.clone(),
        untracked_files: git.untracked_files.clone(),
        conflicted_files: git.conflicted_files.clone(),
        conflict_entries: git.conflict_entries.clone(),
        branch_files: Vec::new(),
        branch_diff_coverage: crate::coverage::Coverage::unrecorded(),
        recent_commits: Vec::new(),
    };
    // Drops Toimija's own generated output, as the packet's tokens do.
    crate::context::refresh_token(&view)
}

/// A probe-proven change detector: the native backend (FSEvents, inotify) when
/// it arms and proves ready; otherwise notify's polling on macOS and Git-state
/// polling on Linux. Watcher payloads are held so dropping the backend
/// unregisters the OS watch.
enum Backend {
    Native(#[allow(dead_code)] notify::RecommendedWatcher),
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Poll(#[allow(dead_code)] notify::PollWatcher),
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    GitPoll,
}

impl Backend {
    fn label(&self) -> &'static str {
        match self {
            Self::Native(_) => "native",
            Self::Poll(_) => "poll fallback",
            Self::GitPoll => "git polling",
        }
    }
}

/// An armed backend and, for the event backends, its event receiver.
struct Armed {
    backend: Backend,
    rx: Option<mpsc::Receiver<notify::Result<Event>>>,
}

/// An active filesystem watch. The fallible OS setup is done in [`start`], so a
/// caller can report a startup failure *before* doing other work; the blocking
/// refresh loop is [`Session::run`]. Dropping the session stops watching.
pub struct Session {
    // Held only to keep the OS watch alive; dropping it unregisters the watch.
    backend: Backend,
    rx: Option<mpsc::Receiver<notify::Result<Event>>>,
    repo: Repository,
    known: Known,
    carried: Carried,
    timing: Timing,
    /// The monotonic clock as the current tick read it: what the backoff and
    /// the loop's cadences count from.
    clock: Instant,
}

/// The Git state the last packet was compiled from, and the refs' stamp from
/// before it was read. While the refs stamp holds, its ref-derived facts — the
/// base, the merge-base, the branch diff, the recent commits — are what a
/// fresh read would find, so a refresh reads only `git status` and carries
/// them ([`git::restatus`]). It is the same trust the watch already puts in
/// the stamp when it decides that nothing moved at all.
#[derive(Debug, Clone, Default)]
struct Carried {
    state: Option<(git::Collected, Vec<Entry>)>,
}

impl Carried {
    fn new(refreshed: &refresh::Refreshed, refs_before: &Stamp) -> Self {
        Self {
            state: Some((refreshed.collected.clone(), refs_before.refs.clone())),
        }
    }

    /// The carried state, when the refs are still where it was read: `stamp`
    /// is taken now.
    fn holding(&self, stamp: &Stamp) -> Option<&git::Collected> {
        self.state
            .as_ref()
            .filter(|(_, refs)| *refs == stamp.refs)
            .map(|(collected, _)| collected)
    }
}

/// What a watch knows when it stops: its repository, and the Git state its
/// last packet was compiled from. `run`'s exit refresh carries it.
pub struct Stopped {
    repo: Repository,
    carried: Carried,
}

impl Stopped {
    /// The worktree the watch read.
    pub fn worktree(&self) -> &git::Worktree {
        self.repo.worktree()
    }

    /// The Git state the last packet was compiled from, when the refs have not
    /// moved since it was read: stamped now.
    pub fn holding(&self) -> Option<&git::Collected> {
        self.carried.holding(&self.repo.stamp())
    }
}

/// Begin watching the discovered repository. The fallible parts — creating the
/// watcher and registering the path — happen here, synchronously, so a startup
/// failure is reported to the caller rather than swallowed by a background
/// loop.
pub fn start(repo: Repository) -> Result<Session> {
    let Armed { backend, rx } = arm(&repo.root)?;
    info!(event = "toimija.watch.armed", backend = backend.label());
    let known = Known::new(repo.discovered.clone());
    Ok(Session {
        backend,
        rx,
        repo,
        known,
        carried: Carried::default(),
        timing: Timing::LIVE,
        clock: Instant::now(),
    })
}

fn arm(repo_root: &Path) -> Result<Armed> {
    start_with_probe(repo_root, prove_ready)
}

fn start_with_probe(
    repo_root: &Path,
    probe: impl FnMut(
        &Path,
        &mpsc::Receiver<notify::Result<Event>>,
        Option<&notify::PollWatcher>,
        &str,
    ) -> Result<bool>,
) -> Result<Armed> {
    let native = arm_watcher::<notify::RecommendedWatcher>(repo_root, Config::default());
    start_from_native(native, repo_root, probe)
}

/// Decide the backend from the native arm attempt: native when it armed *and*
/// proved ready, the fallback otherwise. A native arm failure (e.g. the
/// inotify watch limit) must not abort startup while a fallback can still
/// deliver — it degrades, and says so once in the log.
fn start_from_native(
    native: Result<(
        notify::RecommendedWatcher,
        mpsc::Receiver<notify::Result<Event>>,
    )>,
    repo_root: &Path,
    mut probe: impl FnMut(
        &Path,
        &mpsc::Receiver<notify::Result<Event>>,
        Option<&notify::PollWatcher>,
        &str,
    ) -> Result<bool>,
) -> Result<Armed> {
    let reason = match native {
        Ok((native, rx)) => {
            if probe(repo_root, &rx, None, "native")? {
                return Ok(Armed {
                    backend: Backend::Native(native),
                    rx: Some(rx),
                });
            }
            "it did not report its readiness probe".to_owned()
        }
        Err(error) => format!("{error:#}"),
    };
    let armed = fallback(repo_root, probe)?;
    warn!(
        event = "toimija.watch.native_unavailable",
        reason = %reason,
        fallback = armed.backend.label(),
        "the native file watcher is unavailable; falling back"
    );
    Ok(armed)
}

/// Linux without inotify: Git polling, one `git status` per tick, never a walk
/// of the whole tree through notify's polling backend.
#[cfg(target_os = "linux")]
fn fallback(
    repo_root: &Path,
    _probe: impl FnMut(
        &Path,
        &mpsc::Receiver<notify::Result<Event>>,
        Option<&notify::PollWatcher>,
        &str,
    ) -> Result<bool>,
) -> Result<Armed> {
    fs::metadata(repo_root).with_context(|| format!("failed to watch {}", repo_root.display()))?;
    Ok(Armed {
        backend: Backend::GitPoll,
        rx: None,
    })
}

/// Elsewhere: notify's polling backend, proved ready like the native one.
#[cfg(not(target_os = "linux"))]
fn fallback(
    repo_root: &Path,
    mut probe: impl FnMut(
        &Path,
        &mpsc::Receiver<notify::Result<Event>>,
        Option<&notify::PollWatcher>,
        &str,
    ) -> Result<bool>,
) -> Result<Armed> {
    let (poll, rx) = arm_watcher::<notify::PollWatcher>(repo_root, watcher_config())?;
    let ready = probe(repo_root, &rx, Some(&poll), "poll")?;
    anyhow::ensure!(ready, "filesystem watcher did not report readiness");
    Ok(Armed {
        backend: Backend::Poll(poll),
        rx: Some(rx),
    })
}

fn arm_watcher<W: Watcher>(
    repo_root: &Path,
    config: Config,
) -> Result<(W, mpsc::Receiver<notify::Result<Event>>)> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = W::new(
        move |res: notify::Result<Event>| {
            if res.as_ref().is_ok_and(|event| reads_only(&event.kind)) {
                return;
            }
            let _ = tx.send(res);
        },
        config,
    )
    .context("failed to create filesystem watcher")?;
    watcher
        .watch(repo_root, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", repo_root.display()))?;
    Ok((watcher, rx))
}

/// An event that only says a file was opened, read or closed unwritten.
/// Inotify reports every one, so the refresh's own reads — and any other
/// reader's — would re-trigger it, and the debounce would never go quiet. A
/// closed write is a change and stays.
fn reads_only(kind: &EventKind) -> bool {
    use notify::event::{AccessKind, AccessMode};
    matches!(kind, EventKind::Access(access) if *access != AccessKind::Close(AccessMode::Write))
}

fn prove_ready(
    repo_root: &Path,
    rx: &mpsc::Receiver<notify::Result<Event>>,
    poll: Option<&notify::PollWatcher>,
    backend: &str,
) -> Result<bool> {
    let probe = repo_root.join(format!("{PROBE_PREFIX}{}-{backend}", std::process::id()));
    // Never through a symlink planted at the probe's name.
    crate::owned::open_nofollow(
        &probe,
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true),
    )
    .and_then(|mut file| std::io::Write::write_all(&mut file, b"watch readiness probe"))
    .with_context(|| format!("failed to arm watcher at {}", repo_root.display()))?;
    // Native backends may report the symlink-resolved path — FSEvents turns
    // /tmp/x into /private/tmp/x — so the wait must accept the canonical form
    // of the probe too, or native never proves ready on a symlinked checkout.
    let canonical = fs::canonicalize(&probe).unwrap_or_else(|_| probe.clone());
    let result = wait_for_probe(rx, poll, &probe, &canonical, PROBE_TIMEOUT);
    let _ = fs::remove_file(&probe);
    result
}

fn wait_for_probe(
    rx: &mpsc::Receiver<notify::Result<Event>>,
    poll: Option<&notify::PollWatcher>,
    probe: &Path,
    canonical: &Path,
    timeout: Duration,
) -> Result<bool> {
    if let Some(poll) = poll {
        poll.poll()
            .context("failed to request filesystem watcher readiness poll")?;
    }
    // One total deadline. A per-receive timeout would restart on every event,
    // so steady unrelated churn could stall startup past the fallback window.
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        match rx.recv_timeout(remaining) {
            Ok(Ok(event))
                if event
                    .paths
                    .iter()
                    .any(|path| path == probe || path == canonical) =>
            {
                return Ok(true);
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(false),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(false),
        }
    }
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
fn watcher_config() -> Config {
    Config::default().with_poll_interval(Duration::from_millis(500))
}

impl Session {
    pub fn backend_label(&self) -> &'static str {
        self.backend.label()
    }

    /// Close the gap between `entry` — the packet the caller built, after
    /// discovering the repository and before the watcher was registered — and
    /// the watcher's readiness. One `git status`, compared with the packet
    /// together with the changed files' contents, and the stamp taken at
    /// discovery: a refresh only when something moved, carrying `entry`'s
    /// ref-derived facts when the refs did not. Returns the refreshed packet,
    /// or `None` when `entry` still stands.
    pub fn catch_up(
        &mut self,
        runner: &impl CommandRunner,
        plan: &refresh::Plan,
        entry: &refresh::Refreshed,
    ) -> Result<Option<refresh::Refreshed>> {
        self.known.learn(&entry.packet);
        self.carried = Carried::new(entry, &self.known.stamp);
        let stamp = self.repo.stamp();
        let moved = stamp.refs != self.known.stamp.refs || {
            let observed = self.observe(runner, true)?;
            self.known.differs(&observed)
        };
        self.known.stamp = stamp.clone();
        if !moved {
            return Ok(None);
        }
        debug!(event = "toimija.watch.caught_up");
        let refreshed =
            plan.carrying(runner, self.repo.worktree(), self.carried.holding(&stamp))?;
        self.known.learn(&refreshed.packet);
        self.carried = Carried::new(&refreshed, &stamp);
        Ok(Some(refreshed))
    }

    /// Whether the refs are still where the last packet's Git state was read:
    /// then its ref-derived facts hold.
    pub fn refs_held(&self) -> bool {
        self.carried.holding(&self.repo.stamp()).is_some()
    }

    /// Refresh the live packet on every relevant change until `stop` is set. The
    /// `plan` decides what is written (per-session vs. repo-level). `on_refresh`
    /// is called after each refresh (for status logging). Returns what the
    /// watch knew when it stopped, for a last refresh to carry.
    pub fn run(
        self,
        runner: &impl CommandRunner,
        plan: &refresh::Plan,
        stop: &AtomicBool,
        on_refresh: impl FnMut(),
    ) -> Result<Stopped> {
        self.run_with(runner, plan, stop, on_refresh, ClockPair::now, Timing::LIVE)
    }

    fn run_with(
        mut self,
        runner: &impl CommandRunner,
        plan: &refresh::Plan,
        stop: &AtomicBool,
        mut on_refresh: impl FnMut(),
        mut clocks: impl FnMut() -> ClockPair,
        timing: Timing,
    ) -> Result<Stopped> {
        self.timing = timing;
        let rx = self.rx.take();
        if rx.is_none() && !matches!(self.backend, Backend::GitPoll) {
            anyhow::bail!("filesystem watcher backend missing event receiver");
        }
        let mut previous = clocks();
        let mut last_poll = previous.mono;
        let mut last_net = previous.mono;
        while !stop.load(Ordering::Relaxed) {
            let now = clocks();
            self.clock = now.mono;
            if let Some(slept) = detect_suspension(&previous, &now, SUSPENSION_THRESHOLD) {
                info!(
                    event = "toimija.watch.resumed",
                    slept_secs = slept.as_secs()
                );
                let stamp = self.repo.stamp();
                self.refresh(runner, plan, &mut on_refresh, stamp, None);
                previous = now;
                continue;
            }
            previous = now;
            // What the events since the last tick ask for: a refresh is done
            // here; a check waits for the state check below.
            let mut asked = Change::Ignored;
            match &rx {
                Some(rx) => match rx.recv_timeout(timing.tick) {
                    Ok(Ok(event)) => match self.repo.classify_event(&event) {
                        Change::Ignored => {}
                        first => {
                            let change = self.repo.drain_burst(rx, first);
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                            if change == Change::Tree {
                                let stamp = self.repo.stamp();
                                self.refresh(runner, plan, &mut on_refresh, stamp, None);
                            } else {
                                asked = change;
                            }
                        }
                    },
                    // A watcher-backend error (dropped or rescan-required
                    // events): surface it, since the packet may now be stale,
                    // then keep going. The safety net re-checks.
                    Ok(Err(error)) => self.repo.warn_watcher_error(&error),
                    // Woke to re-check `stop`.
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    // Watcher dropped its sender — nothing more will arrive.
                    // Say so: under `run` the agent keeps working against a
                    // packet that has silently stopped refreshing.
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        eprintln!(
                            "toimija: file watcher stopped; the live packet will no longer refresh"
                        );
                        break;
                    }
                },
                None => std::thread::sleep(timing.tick),
            }
            let poll_due = self.clock.saturating_duration_since(last_poll) >= timing.state_poll;
            if poll_due || asked != Change::Ignored {
                let safety_net =
                    poll_due && self.clock.saturating_duration_since(last_net) >= timing.safety_net;
                if poll_due {
                    last_poll = self.clock;
                }
                if safety_net {
                    last_net = self.clock;
                }
                let check = Check {
                    safety_net,
                    build: asked == Change::Build,
                };
                self.check_state(runner, plan, &mut on_refresh, check);
            }
        }
        Ok(Stopped {
            repo: self.repo,
            carried: self.carried,
        })
    }

    /// Check the Git state against what the packet was built from. With
    /// nothing moved this starts no git process on the event backends, and one
    /// `git status` under Git polling, after a build-directory event, or when
    /// the safety net is due.
    fn check_state(
        &mut self,
        runner: &impl CommandRunner,
        plan: &refresh::Plan,
        on_refresh: &mut impl FnMut(),
        check: Check,
    ) {
        let stamp = self.repo.stamp();
        if self.known.retry_due(self.clock) {
            debug!(event = "toimija.watch.retry_failed_refresh");
            self.refresh(runner, plan, on_refresh, stamp, None);
            return;
        }
        if stamp.refs != self.known.stamp.refs {
            debug!(event = "toimija.watch.refs_moved");
            self.refresh(runner, plan, on_refresh, stamp, None);
            return;
        }
        let status_moved = stamp.status != self.known.stamp.status;
        let git_polling = matches!(self.backend, Backend::GitPoll);
        let deep = status_moved || check.safety_net || check.build;
        if !(deep || git_polling) {
            return;
        }
        match self.observe(runner, deep) {
            Ok(observed) if self.known.adopt_next => {
                self.known.adopt(observed);
                self.known.stamp = stamp;
            }
            Ok(observed) if self.known.differs(&observed) => {
                debug!(event = "toimija.watch.state_poll_changed");
                self.refresh(runner, plan, on_refresh, stamp, Some(observed));
            }
            // Rewritten without a visible change (another tool's `git status`
            // refreshing the index, say), or untracked build output: nothing
            // to refresh.
            Ok(_) => self.known.stamp = stamp,
            Err(error) => {
                eprintln!("toimija: Git-state poll failed (packet may be stale): {error:#}");
            }
        }
    }

    /// One `git status`, against the git dir found at discovery. `deep` adds
    /// the consistency token, which also reads the changed files' contents.
    fn observe(&self, runner: &impl CommandRunner, deep: bool) -> Result<Observed> {
        let mut live =
            git::token_state_at(runner, &self.repo.root, Some(self.repo.git_dir.clone()))?;
        crate::context::normalize_git_state(&mut live);
        Ok(Observed {
            status: status_key(&live),
            consistency: deep.then(|| Consistency::fingerprint(&live).token),
        })
    }

    /// Refresh, having stamped the Git files *before* the refresh reads them:
    /// a change landing while it runs moves the next stamp. While the refs
    /// stamp holds, the refresh reads `git status` and carries the last
    /// packet's ref-derived facts ([`Carried`]).
    fn refresh(
        &mut self,
        runner: &impl CommandRunner,
        plan: &refresh::Plan,
        on_refresh: &mut impl FnMut(),
        stamp: Stamp,
        observed: Option<Observed>,
    ) {
        let carried = self.carried.holding(&stamp);
        match refresh_or_warn(runner, self.repo.worktree(), carried, plan, on_refresh) {
            Some(refreshed) => {
                self.known.learn(&refreshed.packet);
                self.carried = Carried::new(&refreshed, &stamp);
            }
            None => self.known.fail(observed, self.timing, self.clock),
        }
        self.known.stamp = stamp;
    }
}

/// What a state check is asked to look at beyond the stamp.
#[derive(Debug, Clone, Copy)]
struct Check {
    /// The safety net is due: compare one `git status` and the changed files'
    /// contents with the packet.
    safety_net: bool,
    /// Something moved in a build directory at the root: the same comparison,
    /// which shows a tracked file there as modified.
    build: bool,
}

/// A transient refresh failure must not kill live updates: keep watching so
/// the packet can recover on the next event, the safety net, or resume. But
/// the agent reads the packet, not this process's stderr, so the failure goes
/// into the packet itself (issue #229) — once per distinct failure, not on
/// every retry.
fn refresh_or_warn(
    runner: &impl CommandRunner,
    worktree: &git::Worktree,
    carried: Option<&git::Collected>,
    plan: &refresh::Plan,
    on_refresh: &mut impl FnMut(),
) -> Option<refresh::Refreshed> {
    let repo_root = &worktree.root;
    match plan.carrying(runner, worktree, carried) {
        Ok(refreshed) => {
            on_refresh();
            Some(refreshed)
        }
        Err(error) => {
            match plan.mark_stale(repo_root, &error) {
                Ok(true) => {
                    eprintln!("toimija: refresh failed; the packet now says it is stale: {error:#}")
                }
                // Already said, in the packet and here: a retry is not news.
                Ok(false) => {}
                Err(mark) => eprintln!(
                    "toimija: refresh failed (packet may be stale): {error:#}; \
                     marking the packet stale also failed: {mark:#}"
                ),
            }
            None
        }
    }
}

/// Convenience for the standalone `watch` command: stamp the discovered
/// repository, baseline refresh, then start and catch up after registration
/// before running the loop. Startup failures propagate via `?`.
pub fn watch(
    runner: &impl CommandRunner,
    worktree: &git::Worktree,
    plan: &refresh::Plan,
    stop: &AtomicBool,
    on_refresh: impl FnMut(),
) -> Result<()> {
    let repo = Repository::of(runner, worktree)?;
    // Baseline so the live packet exists immediately, before any event.
    let baseline = plan.run(runner, worktree)?;
    let mut session = start(repo)?;
    session.catch_up(runner, plan, &baseline)?;
    eprintln!("toimija watch: armed ({})", session.backend_label());
    session.run(runner, plan, stop, on_refresh).map(|_| ())
}

/// Whether a path relative to the repository root is Toimija's own: generated
/// local tool output, its Git footprint, or its readiness probe. Matched
/// relative to the root only: a repository that itself lives under a
/// `target/` or `.sykli/` directory is not ignored.
fn is_ignored(relative: &Path) -> bool {
    consistency::is_generated_output(relative)
        || is_toimija_git_footprint(relative)
        || first_component(relative).is_some_and(|name| name.starts_with(PROBE_PREFIX))
}

/// Whether a path relative to the repository root lies in a build directory
/// at the root. Only there: `lib/deps/` is source.
fn is_build_output(relative: &Path) -> bool {
    first_component(relative).is_some_and(|name| BUILD_DIRS.contains(&name))
}

/// Toimija's own Git writes must not refresh the packet that caused them.
/// Keep HEAD, index, reflogs, and packed refs relevant: they can describe a
/// real repository transition rather than observer bookkeeping.
fn is_toimija_git_footprint(relative: &Path) -> bool {
    let mut parts = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        });
    match (parts.next(), parts.next(), parts.next()) {
        (Some(".git"), Some("objects"), _) => true,
        (Some(".git"), Some("refs"), Some("toimija")) => true,
        (Some(".git"), Some(name), _) => name.starts_with("toimija-snapshot-"),
        _ => false,
    }
}

fn first_component(relative: &Path) -> Option<&str> {
    match relative.components().next() {
        Some(Component::Normal(name)) => name.to_str(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandOutput, ProcessCommandRunner};
    use std::cell::{Cell, RefCell};
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    /// The git call that marks a refresh reading the refs: the recent history.
    const READS_THE_REFS: [&str; 4] = ["log", "-n", "10", "--oneline"];

    struct RefreshCountingRunner {
        refreshes: Cell<usize>,
    }

    struct FailingRunner {
        attempts: Cell<usize>,
    }

    impl CommandRunner for RefreshCountingRunner {
        fn run(&self, _cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            let (status, stdout) = match (program, args) {
                ("git", ["--no-optional-locks", "status", ..]) => {
                    // A plan run collects Git status once, so this is the
                    // refresh-count proxy asserted below.
                    self.refreshes.set(self.refreshes.get() + 1);
                    (0, Vec::new())
                }
                _ => (1, Vec::new()),
            };
            Ok(CommandOutput {
                status,
                stdout,
                stderr: Vec::new(),
            })
        }
    }

    impl CommandRunner for FailingRunner {
        fn run(
            &self,
            _cwd: &Path,
            _program: &str,
            _args: &[&str],
        ) -> std::io::Result<CommandOutput> {
            self.attempts.set(self.attempts.get() + 1);
            Err(std::io::Error::other("fabricated refresh failure"))
        }
    }

    /// The real git, with every process it starts — its children included —
    /// recorded by Git's own trace2 event stream, the way an operator measures
    /// a watch: `GIT_TRACE2_EVENT=<file>`, counting `"event":"start"`.
    struct Traced {
        trace: PathBuf,
        calls: RefCell<Vec<Vec<String>>>,
        /// When each refresh began.
        attempts: RefCell<Vec<Instant>>,
    }

    impl Traced {
        fn new(dir: &Path) -> Self {
            Self {
                trace: dir.join("trace2.json"),
                calls: RefCell::new(Vec::new()),
                attempts: RefCell::new(Vec::new()),
            }
        }

        /// Git processes started so far.
        fn starts(&self) -> usize {
            fs::read_to_string(&self.trace)
                .map(|trace| {
                    trace
                        .lines()
                        .filter(|line| line.contains(r#""event":"start""#))
                        .count()
                })
                .unwrap_or(0)
        }

        /// Refreshes attempted so far that read the refs: each reads the recent
        /// history. A refresh that carries them, the refs unmoved, does not.
        fn refreshes(&self) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|args| args[..] == READS_THE_REFS)
                .count()
        }
    }

    impl CommandRunner for Traced {
        fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            self.run_with_env(cwd, program, args, &[])
        }

        fn run_with_env(
            &self,
            cwd: &Path,
            program: &str,
            args: &[&str],
            env: &[(&str, &OsStr)],
        ) -> std::io::Result<CommandOutput> {
            if args == READS_THE_REFS {
                self.attempts.borrow_mut().push(Instant::now());
            }
            self.calls
                .borrow_mut()
                .push(args.iter().map(|arg| arg.to_string()).collect());
            let mut env = env.to_vec();
            env.push(("GIT_TRACE2_EVENT", self.trace.as_os_str()));
            ProcessCommandRunner.run_with_env(cwd, program, args, &env)
        }
    }

    /// Fixture git, never traced. Scrub the redirecting variables: a value
    /// this process inherits (a hook's `GIT_DIR`, say) would point a fixture
    /// `git` at that repository instead of the fixture's.
    fn fixture_git(dir: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_TRACE2_EVENT")
            .output()
            .expect("git runs")
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = fixture_git(dir, args);
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_usable() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    /// A repository with one commit on `main`, at its canonical path — the
    /// spelling `git rev-parse --show-toplevel` gives the callers.
    fn repo_with_commit() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        init(&root);
        fs::write(root.join("a.txt"), "x\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "init"]);
        (temp, root)
    }

    fn init(root: &Path) {
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.email", "toimija@example.invalid"]);
        git(root, &["config", "user.name", "Toimija Test"]);
        git(root, &["config", "commit.gpgsign", "false"]);
    }

    fn plan() -> refresh::Plan {
        refresh::Plan {
            for_diff: false,
            budget: None,
            session_dir: None,
        }
    }

    /// A session over `repo` with no OS watcher behind it: Git polling, or an
    /// event backend whose channel stays open and silent — what an event
    /// backend looks like while nothing in the work tree moves.
    fn bare_session(
        repo: Repository,
        git_polling: bool,
    ) -> (Session, Option<mpsc::Sender<notify::Result<Event>>>) {
        let known = Known::new(repo.discovered.clone());
        if git_polling {
            let session = Session {
                backend: Backend::GitPoll,
                rx: None,
                repo,
                known,
                carried: Carried::default(),
                timing: Timing::LIVE,
                clock: Instant::now(),
            };
            return (session, None);
        }
        let watcher = notify::PollWatcher::new(|_| {}, watcher_config()).unwrap();
        let (tx, rx) = mpsc::channel();
        let session = Session {
            backend: Backend::Poll(watcher),
            rx: Some(rx),
            repo,
            known,
            carried: Carried::default(),
            timing: Timing::LIVE,
            clock: Instant::now(),
        };
        (session, Some(tx))
    }

    /// Discover, build the baseline packet, and catch up, as `watch` and
    /// `run` do.
    fn armed(
        runner: &Traced,
        root: &Path,
        git_polling: bool,
    ) -> (Session, Option<mpsc::Sender<notify::Result<Event>>>) {
        let repo = Repository::discover(runner, root).unwrap();
        let baseline = plan().run(runner, repo.worktree()).unwrap();
        let (mut session, tx) = bare_session(repo, git_polling);
        assert!(
            session
                .catch_up(runner, &plan(), &baseline)
                .unwrap()
                .is_none(),
            "nothing moved since the baseline"
        );
        (session, tx)
    }

    const FAST: Timing = Timing {
        tick: Duration::from_millis(2),
        state_poll: Duration::ZERO,
        safety_net: Duration::from_secs(3600),
        retry_first: Duration::from_secs(3600),
    };

    /// Run the real loop for `ticks` ticks, each one a state check; returns
    /// the refreshes it made.
    fn run_ticks(
        session: Session,
        runner: &impl CommandRunner,
        ticks: usize,
        timing: Timing,
    ) -> usize {
        let stop = AtomicBool::new(false);
        let mut refreshes = 0;
        let mut clock_reads = 0;
        session
            .run_with(
                runner,
                &plan(),
                &stop,
                || refreshes += 1,
                || {
                    // The first read is the loop's baseline; each later one
                    // starts a tick.
                    clock_reads += 1;
                    if clock_reads > ticks {
                        stop.store(true, Ordering::Relaxed);
                    }
                    ClockPair::now()
                },
                timing,
            )
            .unwrap();
        refreshes
    }

    fn idle_session() -> Session {
        let watcher = notify::PollWatcher::new(|_| {}, watcher_config()).unwrap();
        let (tx, rx) = mpsc::channel();
        // The dropped sender makes the loop exit on its first quiet tick.
        drop(tx);
        let repo = Repository::at(
            Path::new("/repo"),
            PathBuf::from("/repo/.git"),
            PathBuf::from("/repo/.git"),
        );
        Session {
            backend: Backend::Poll(watcher),
            rx: Some(rx),
            known: Known::new(repo.discovered.clone()),
            repo,
            carried: Carried::default(),
            timing: Timing::LIVE,
            clock: Instant::now(),
        }
    }

    fn repo_at(root: &str) -> Repository {
        Repository::at(
            Path::new(root),
            Path::new(root).join(".git"),
            Path::new(root).join(".git"),
        )
    }

    fn is_ignored_at(root: &str, path: &str) -> bool {
        repo_at(root).classify(Path::new(path)) == Change::Ignored
    }

    #[test]
    fn an_idle_tick_starts_no_git_process_on_an_event_backend() {
        if !git_usable() {
            return;
        }
        let (temp, root) = repo_with_commit();
        let runner = Traced::new(temp.path());
        let (session, _events) = armed(&runner, &root, false);
        let before = runner.starts();

        let refreshes = run_ticks(session, &runner, 20, FAST);

        let during = runner.starts() - before;
        assert!(
            during == 0,
            "20 idle ticks started {during} git processes: {:?}",
            runner.calls.borrow()
        );
        assert_eq!(refreshes, 0);
    }

    #[test]
    fn an_idle_tick_starts_one_git_process_under_git_polling() {
        if !git_usable() {
            return;
        }
        let (temp, root) = repo_with_commit();
        let runner = Traced::new(temp.path());
        let (session, _) = armed(&runner, &root, true);
        let before = runner.starts();

        let refreshes = run_ticks(session, &runner, 20, FAST);

        // Git polling has no events: the tick's one `git status` is how it
        // sees the work tree.
        let during = runner.starts() - before;
        assert!(
            during <= 20,
            "20 idle ticks started {during} git processes: {:?}",
            runner.calls.borrow()
        );
        assert_eq!(refreshes, 0);
    }

    #[test]
    fn an_idle_watch_on_this_platforms_backend_stays_within_budget() {
        if !git_usable() {
            return;
        }
        let (temp, root) = repo_with_commit();
        let runner = Traced::new(temp.path());
        let repo = Repository::discover(&runner, &root).unwrap();
        let baseline = plan().run(&runner, repo.worktree()).unwrap();
        let mut session = start(repo).unwrap();
        let before_catch_up = runner.starts();
        assert!(
            session
                .catch_up(&runner, &plan(), &baseline)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            runner.starts() - before_catch_up,
            1,
            "catching up is one `git status`"
        );
        let per_tick = usize::from(matches!(session.backend, Backend::GitPoll));
        let before = runner.starts();

        let refreshes = run_ticks(session, &runner, 20, FAST);

        let during = runner.starts() - before;
        assert!(
            during <= 20 * per_tick + 1,
            "20 idle ticks started {during} git processes: {:?}",
            runner.calls.borrow()
        );
        assert_eq!(refreshes, 0, "nothing moved, not even the readiness probe");
    }

    #[test]
    fn toimijas_own_refresh_moves_no_stamp() {
        // The session refresh writes a tree through a scratch index in the git
        // dir, objects, and the packet. None of it may move the stamp, or the
        // watch would refresh on its own refreshes.
        if !git_usable() {
            return;
        }
        let (_temp, root) = repo_with_commit();
        let runner = ProcessCommandRunner;
        let repo = Repository::discover(&runner, &root).unwrap();
        let before = repo.stamp();
        let session_plan = refresh::Plan {
            session_dir: Some(root.join(".toimija/sessions/test")),
            ..plan()
        };
        session_plan.run(&runner, repo.worktree()).unwrap();
        plan().run(&runner, repo.worktree()).unwrap();

        assert_eq!(repo.stamp(), before);
    }

    #[test]
    fn the_stamp_sees_each_kind_of_git_change_without_a_git_process() {
        if !git_usable() {
            return;
        }
        let (_temp, root) = repo_with_commit();
        let repo = Repository::discover(&ProcessCommandRunner, &root).unwrap();
        let mut last = repo.stamp();
        let mut step = |what: &str, status: bool, refs: bool, change: &dyn Fn()| {
            change();
            let now = repo.stamp();
            assert_eq!(now.refs != last.refs, refs, "{what}: refs half");
            // A moved refs half refreshes whatever the status half did.
            if !refs {
                assert_eq!(now.status != last.status, status, "{what}: status half");
            }
            last = now;
        };
        step("staging", true, false, &|| {
            fs::write(root.join("a.txt"), "y\n").unwrap();
            git(&root, &["add", "a.txt"]);
        });
        step("a commit", true, true, &|| {
            git(&root, &["commit", "-q", "-m", "second"]);
        });
        step("an empty commit", false, true, &|| {
            git(&root, &["commit", "-q", "--allow-empty", "-m", "third"]);
        });
        step("a branch elsewhere", false, true, &|| {
            git(&root, &["branch", "elsewhere"]);
        });
        step("a branch switch", true, false, &|| {
            git(&root, &["switch", "-q", "elsewhere"]);
        });
        step("an upstream", false, true, &|| {
            git(
                &root,
                &["config", "branch.elsewhere.merge", "refs/heads/main"],
            );
        });
        step("packing refs", false, true, &|| {
            git(&root, &["pack-refs", "--all"]);
        });
        step("the packet's configuration", false, true, &|| {
            fs::create_dir_all(root.join(".toimija")).unwrap();
            fs::write(root.join(crate::config::CONFIG_PATH), "repo:\n  name: x\n").unwrap();
        });
        step("a work-tree edit", false, false, &|| {
            fs::write(root.join("a.txt"), "z\n").unwrap();
        });
    }

    #[test]
    fn packet_and_live_probe_agree_in_every_head_and_file_state() {
        // The churn guard: the packet-side keys and the live probe must agree
        // on an untouched repo, or every probe would refresh, forever —
        // invisible to the change tests, which only assert that a refresh
        // *happens*. Going through the real packet build pins the chain.
        if !git_usable() {
            return;
        }
        let assert_agree = |what: &str, root: &Path| {
            let runner = ProcessCommandRunner;
            let packet = crate::context::build_context_packet(&runner, root, false).unwrap();
            let repo = Repository::discover(&runner, root).unwrap();
            let (session, _events) = bare_session(repo, true);
            let mut known = Known::new(Stamp::default());
            known.learn(&packet);
            let observed = session.observe(&runner, true).unwrap();
            assert!(
                !known.differs(&observed),
                "{what}: packet {:?}/{:?}, live {observed:?}",
                known.status,
                known.consistency
            );
        };

        let (_temp, root) = repo_with_commit();
        assert_agree("clean", &root);
        fs::write(root.join("staged.txt"), "s\n").unwrap();
        git(&root, &["add", "staged.txt"]);
        fs::write(root.join("a.txt"), "changed\n").unwrap();
        fs::write(root.join("untracked.txt"), "u\n").unwrap();
        assert_agree("dirty", &root);
        git(&root, &["stash", "-q", "--include-untracked"]);
        git(&root, &["checkout", "-q", "--detach"]);
        assert_agree("detached", &root);

        // A merge stopped on a conflict: the watch must sit still through it.
        git(&root, &["switch", "-q", "-c", "left", "main"]);
        fs::write(root.join("a.txt"), "left\n").unwrap();
        git(&root, &["commit", "-q", "-am", "left"]);
        git(&root, &["switch", "-q", "-c", "right", "main"]);
        fs::write(root.join("a.txt"), "right\n").unwrap();
        git(&root, &["commit", "-q", "-am", "right"]);
        assert!(
            !fixture_git(&root, &["merge", "-q", "left"])
                .status
                .success()
        );
        assert_agree("conflicted", &root);

        let unborn = tempfile::tempdir().unwrap();
        let unborn_root = fs::canonicalize(unborn.path()).unwrap();
        init(&unborn_root);
        fs::write(unborn_root.join("new.txt"), "n\n").unwrap();
        assert_agree("unborn", &unborn_root);

        // A linked worktree tracking an upstream, its git dir outside it.
        let (temp, origin) = repo_with_commit();
        let clone = temp.path().join("clone");
        git(
            temp.path(),
            &["clone", "-q", origin.to_str().unwrap(), "clone"],
        );
        let linked = fs::canonicalize(temp.path()).unwrap().join("linked");
        git(
            &clone,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature",
                linked.to_str().unwrap(),
                "origin/main",
            ],
        );
        git(
            &linked,
            &["branch", "-q", "--set-upstream-to", "origin/main"],
        );
        fs::write(linked.join("b.txt"), "b\n").unwrap();
        assert_agree("linked worktree with upstream", &linked);
    }

    #[test]
    fn catch_up_refreshes_a_change_made_before_the_watcher_armed() {
        if !git_usable() {
            return;
        }
        let (temp, root) = repo_with_commit();
        let runner = Traced::new(temp.path());
        let repo = Repository::discover(&runner, &root).unwrap();
        let baseline = plan().run(&runner, repo.worktree()).unwrap();
        // Between the baseline packet and the watcher's registration.
        fs::write(root.join("late.txt"), "l\n").unwrap();
        let (mut session, _events) = bare_session(repo, false);

        let caught_up = session.catch_up(&runner, &plan(), &baseline).unwrap();

        let packet = caught_up
            .expect("a work-tree change before arming is caught up")
            .packet;
        assert!(
            packet
                .git
                .untracked_files
                .contains(&PathBuf::from("late.txt"))
        );

        // A ref moved before arming: refreshed without the probe.
        let repo = Repository::discover(&runner, &root).unwrap();
        let baseline = plan().run(&runner, repo.worktree()).unwrap();
        git(&root, &["branch", "moved"]);
        let (mut session, _events) = bare_session(repo, false);
        assert!(
            session
                .catch_up(&runner, &plan(), &baseline)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn staging_and_a_commit_refresh_without_any_filesystem_event() {
        // A linked worktree's git dir is outside the watched root, so staging
        // and committing there reach no event backend: the stamp sees them.
        if !git_usable() {
            return;
        }
        let (temp, root) = repo_with_commit();
        let runner = Traced::new(temp.path());
        fs::write(root.join("a.txt"), "edited\n").unwrap();
        let (session, _events) = armed(&runner, &root, false);
        git(&root, &["add", "a.txt"]);
        assert_eq!(run_ticks(session, &runner, 1, FAST), 1, "staging refreshes");
        let packet = fs::read_to_string(root.join(refresh::CURRENT_JSON)).unwrap();
        assert!(packet.contains(r#""staged_files": ["#), "{packet}");

        let (session, _events) = armed(&runner, &root, false);
        git(&root, &["commit", "-q", "-m", "staged"]);
        let refreshes = runner.refreshes();
        assert_eq!(
            run_ticks(session, &runner, 1, FAST),
            1,
            "a commit refreshes"
        );
        assert_eq!(runner.refreshes(), refreshes + 1);
    }

    #[test]
    fn an_index_rewrite_that_changes_nothing_costs_one_status_and_no_refresh() {
        // Agents and editors run `git status`, which rewrites the index to
        // refresh its stat data. The packet does not change; one `git status`
        // proves it.
        if !git_usable() {
            return;
        }
        let (temp, root) = repo_with_commit();
        let runner = Traced::new(temp.path());
        let (session, _events) = armed(&runner, &root, false);
        let stamp = session.repo.stamp();
        // Same bytes, a later mtime: the index's stat data goes stale.
        std::thread::sleep(Duration::from_millis(20));
        fs::write(root.join("a.txt"), "x\n").unwrap();
        git(&root, &["update-index", "--refresh"]);
        assert_ne!(session.repo.stamp().status, stamp.status, "precondition");
        let before = runner.starts();

        assert_eq!(run_ticks(session, &runner, 3, FAST), 0);
        assert_eq!(runner.starts() - before, 1);
    }

    #[test]
    fn the_safety_net_catches_what_no_event_or_stamp_reports() {
        // A second edit to an already-modified file: `git status` reads the
        // same, and only the file's contents changed. Under Git polling no
        // event reports it; the safety net's consistency check does.
        if !git_usable() {
            return;
        }
        let (temp, root) = repo_with_commit();
        let runner = Traced::new(temp.path());
        fs::write(root.join("a.txt"), "first edit\n").unwrap();
        let (session, _) = armed(&runner, &root, true);
        fs::write(root.join("a.txt"), "second edit\n").unwrap();
        assert_eq!(run_ticks(session, &runner, 3, FAST), 0, "not seen per tick");

        let (session, _) = armed(&runner, &root, true);
        fs::write(root.join("a.txt"), "third edit\n").unwrap();
        let net = Timing {
            safety_net: Duration::ZERO,
            ..FAST
        };
        assert_eq!(run_ticks(session, &runner, 1, net), 1, "seen by the net");
    }

    #[test]
    fn a_failing_refresh_is_not_retried_on_every_tick() {
        if !git_usable() {
            return;
        }
        let (temp, root) = repo_with_commit();
        let runner = Traced::new(temp.path());
        let (session, _) = armed(&runner, &root, true);
        // An unparseable configuration: every refresh fails until it is fixed.
        fs::create_dir_all(root.join(".toimija")).unwrap();
        fs::write(root.join(crate::config::CONFIG_PATH), "repo: [\n").unwrap();
        let before = runner.refreshes();

        assert_eq!(run_ticks(session, &runner, 10, FAST), 0);

        // Once, for the moved configuration: the status that moved with it is
        // taken as seen, and the backoff's first retry is not due yet.
        assert_eq!(runner.refreshes() - before, 1);
    }

    #[test]
    fn suspension_is_detected_when_wall_time_outruns_the_monotonic_clock() {
        let mono = Instant::now();
        let prev = ClockPair {
            wall: UNIX_EPOCH,
            mono,
        };
        let now = ClockPair {
            wall: UNIX_EPOCH + Duration::from_secs(61),
            mono: mono + Duration::from_secs(1),
        };

        assert_eq!(
            detect_suspension(&prev, &now, SUSPENSION_THRESHOLD),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn scheduler_jitter_under_the_threshold_is_not_a_suspension() {
        let mono = Instant::now();
        let prev = ClockPair {
            wall: UNIX_EPOCH,
            mono,
        };
        let now = ClockPair {
            wall: UNIX_EPOCH + Duration::from_secs(30),
            mono: mono + Duration::from_secs(1),
        };

        assert_eq!(detect_suspension(&prev, &now, SUSPENSION_THRESHOLD), None);
    }

    #[test]
    fn a_forward_wall_clock_step_over_threshold_is_a_suspension() {
        let mono = Instant::now();
        let prev = ClockPair {
            wall: UNIX_EPOCH,
            mono,
        };
        let now = ClockPair {
            wall: UNIX_EPOCH + Duration::from_secs(31),
            mono,
        };

        assert_eq!(
            detect_suspension(&prev, &now, SUSPENSION_THRESHOLD),
            Some(Duration::from_secs(31))
        );
    }

    #[test]
    fn a_wall_clock_set_backwards_never_fires() {
        let mono = Instant::now();
        let prev = ClockPair {
            wall: UNIX_EPOCH + Duration::from_secs(60),
            mono,
        };
        let now = ClockPair {
            wall: UNIX_EPOCH,
            mono: mono + Duration::from_secs(60),
        };

        assert_eq!(detect_suspension(&prev, &now, SUSPENSION_THRESHOLD), None);
    }

    fn run_with_schedule(
        session: Session,
        runner: &impl CommandRunner,
        root: &Path,
        schedule: Vec<ClockPair>,
        stop_on_refresh: bool,
    ) -> usize {
        let stop = AtomicBool::new(false);
        let mut clocks = schedule.into_iter();
        let mut heartbeats = 0;
        let mut session = session;
        session.repo = Repository::at(root, root.join(".git"), root.join(".git"));
        session
            .run_with(
                runner,
                &plan(),
                &stop,
                || {
                    heartbeats += 1;
                    if stop_on_refresh {
                        stop.store(true, Ordering::Relaxed);
                    }
                },
                || {
                    clocks
                        .next()
                        .expect("loop ran more ticks than the fabricated schedule")
                },
                Timing::LIVE,
            )
            .unwrap();
        heartbeats
    }

    #[test]
    fn resume_triggers_exactly_one_refresh_and_heartbeat() {
        let temp = tempfile::tempdir().unwrap();
        let runner = RefreshCountingRunner {
            refreshes: Cell::new(0),
        };
        let mono = Instant::now();
        let schedule = vec![
            ClockPair {
                wall: UNIX_EPOCH,
                mono,
            },
            ClockPair {
                wall: UNIX_EPOCH + Duration::from_secs(61),
                mono: mono + Duration::from_secs(1),
            },
        ];

        let heartbeats = run_with_schedule(idle_session(), &runner, temp.path(), schedule, true);

        assert_eq!(runner.refreshes.get(), 1);
        assert_eq!(heartbeats, 1);
    }

    #[test]
    fn jitter_under_threshold_triggers_no_refresh() {
        let temp = tempfile::tempdir().unwrap();
        let runner = RefreshCountingRunner {
            refreshes: Cell::new(0),
        };
        let mono = Instant::now();
        let schedule = vec![
            ClockPair {
                wall: UNIX_EPOCH,
                mono,
            },
            ClockPair {
                wall: UNIX_EPOCH + Duration::from_secs(30),
                mono: mono + Duration::from_secs(1),
            },
        ];

        let heartbeats = run_with_schedule(idle_session(), &runner, temp.path(), schedule, false);

        assert_eq!(runner.refreshes.get(), 0);
        assert_eq!(heartbeats, 0);
    }

    #[test]
    fn a_failed_resume_refresh_keeps_watching() {
        let temp = tempfile::tempdir().unwrap();
        let runner = FailingRunner {
            attempts: Cell::new(0),
        };
        let mono = Instant::now();
        let schedule = vec![
            ClockPair {
                wall: UNIX_EPOCH,
                mono,
            },
            ClockPair {
                wall: UNIX_EPOCH + Duration::from_secs(61),
                mono: mono + Duration::from_secs(1),
            },
            ClockPair {
                wall: UNIX_EPOCH + Duration::from_secs(61),
                mono: mono + Duration::from_secs(1),
            },
        ];

        let heartbeats = run_with_schedule(idle_session(), &runner, temp.path(), schedule, false);

        assert_eq!(runner.attempts.get(), 1);
        assert_eq!(heartbeats, 0);
    }

    #[test]
    fn a_failed_refresh_learns_no_packet() {
        let temp = tempfile::tempdir().unwrap();
        let runner = FailingRunner {
            attempts: Cell::new(0),
        };
        let mut heartbeats = 0;

        let refreshed = refresh_or_warn(
            &runner,
            &git::Worktree::at_root(temp.path().to_path_buf()),
            None,
            &plan(),
            &mut || heartbeats += 1,
        );

        assert!(refreshed.is_none());
        assert_eq!(heartbeats, 0);
    }

    #[test]
    fn a_failed_refresh_says_so_in_the_packet_once_and_keeps_the_last_good_content() {
        let temp = tempfile::tempdir().unwrap();
        let md = temp.path().join(refresh::CURRENT_MD);
        std::fs::create_dir_all(md.parent().unwrap()).unwrap();
        std::fs::write(&md, "# Toimija Context\n- HEAD: abc123\n").unwrap();
        let runner = FailingRunner {
            attempts: Cell::new(0),
        };
        let plan = plan();

        refresh_or_warn(
            &runner,
            &git::Worktree::at_root(temp.path().to_path_buf()),
            None,
            &plan,
            &mut || {},
        );
        let marked = std::fs::read_to_string(&md).unwrap();
        assert!(
            marked.contains("Refresh failed — this packet is stale."),
            "{marked}"
        );
        assert!(
            marked.ends_with("# Toimija Context\n- HEAD: abc123\n"),
            "the last good packet is kept under the notice: {marked}"
        );

        // The same failure on the next poll is not written again.
        let error = anyhow::anyhow!("failed to build context packet: bad config");
        assert!(plan.mark_stale(temp.path(), &error).unwrap());
        assert!(!plan.mark_stale(temp.path(), &error).unwrap());
        // A different failure replaces the notice instead of stacking.
        let other = anyhow::anyhow!("another failure");
        assert!(plan.mark_stale(temp.path(), &other).unwrap());
        let replaced = std::fs::read_to_string(&md).unwrap();
        assert_eq!(replaced.matches("Refresh failed").count(), 1, "{replaced}");
        assert!(replaced.contains("another failure"), "{replaced}");
        assert!(replaced.ends_with("- HEAD: abc123\n"), "{replaced}");
    }

    #[test]
    fn live_files_are_ignored() {
        assert!(is_ignored_at("/repo", "/repo/.toimija/current.md"));
        assert!(is_ignored_at("/repo", "/repo/.toimija/current.json"));
        assert!(is_ignored_at("/repo", "/repo/.teko/teko.redb"));
        // A gate's receipts: generated state the token no longer watches.
        assert!(is_ignored_at("/repo", "/repo/.sykli/receipt.json"));
        assert!(is_ignored_at("/repo", "/repo/.sykli/runs/r1/journal"));
        // repo.yaml under .toimija is NOT a live file — changes to it matter.
        assert!(!is_ignored_at("/repo", "/repo/.toimija/repo.yaml"));
    }

    #[test]
    fn per_session_output_is_ignored_but_repo_yaml_is_not() {
        // The watcher writes per-session packets; those writes must not re-trigger.
        assert!(is_ignored_at(
            "/repo",
            "/repo/.toimija/sessions/1782608449-66987/current.md"
        ));
        assert!(is_ignored_at(
            "/repo",
            "/repo/.toimija/sessions/1782608449-66987/session.json"
        ));
        // Config under .toimija still matters and must NOT be ignored.
        assert!(!is_ignored_at("/repo", "/repo/.toimija/repo.yaml"));
    }

    #[test]
    fn runs_output_is_ignored_but_repo_yaml_is_not() {
        assert!(is_ignored_at("/repo", "/repo/.toimija/runs/kisko.jsonl"));
        assert!(!is_ignored_at("/repo", "/repo/.toimija/repo.yaml"));
    }

    #[test]
    fn build_output_at_the_root_is_checked_and_nested_build_names_are_source() {
        // A tracked file is never ignored. Build output at the root costs one
        // `git status`, which shows a tracked file there as modified; a
        // directory of the same name anywhere else is ordinary source.
        let repo = repo_at("/repo");
        for build in [
            "/repo/target/debug/foo",
            "/repo/_build/dev/app.beam",
            "/repo/deps/plug/mix.exs",
            "/repo/node_modules/x.js",
        ] {
            assert_eq!(repo.classify(Path::new(build)), Change::Build, "{build}");
        }
        for source in [
            "/repo/lib/deps/x.rs",
            "/repo/apps/web/node_modules/x.js",
            "/repo/src/target/mod.rs",
        ] {
            assert_eq!(repo.classify(Path::new(source)), Change::Tree, "{source}");
        }
    }

    #[test]
    fn a_repository_under_an_ignored_name_still_sees_its_own_events() {
        // Matching is relative to the root: a checkout that lives under a
        // `target/`, `deps/` or `.sykli/` directory is not ignored wholesale.
        for root in [
            "/work/target/repo",
            "/home/me/deps/repo",
            "/srv/.sykli/repo",
            "/x/.toimija/repo",
        ] {
            let repo = repo_at(root);
            let source = Path::new(root).join("src/main.rs");
            assert_eq!(repo.classify(&source), Change::Tree, "{root}");
            assert_eq!(
                repo.classify(&Path::new(root).join(".git/HEAD")),
                Change::Git,
                "{root}"
            );
            assert_eq!(
                repo.classify(&Path::new(root).join(".toimija/current.md")),
                Change::Ignored,
                "{root}"
            );
            assert_eq!(
                repo.classify(&Path::new(root).join("target/debug/x")),
                Change::Build,
                "{root}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn events_under_the_canonical_spelling_of_the_root_are_matched() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let repo = Repository::at(&link, link.join(".git"), link.join(".git"));
        let canonical = fs::canonicalize(&real).unwrap();

        assert_eq!(repo.classify(&canonical.join("src/lib.rs")), Change::Tree);
        assert_eq!(
            repo.classify(&canonical.join(".toimija/current.md")),
            Change::Ignored
        );
        // Neither spelling: a stamp check, never a refresh by itself.
        assert_eq!(
            repo.classify(Path::new("/elsewhere/src/lib.rs")),
            Change::Git
        );
    }

    #[test]
    fn the_readiness_probe_is_ignored() {
        let probe = format!("/repo/{PROBE_PREFIX}123-native");
        assert!(is_ignored_at("/repo", &probe));
        assert!(!is_ignored_at(
            "/repo",
            "/repo/src/toimija-watch-probe-1-native"
        ));
    }

    #[test]
    fn source_is_a_tree_change_and_git_state_a_git_change() {
        let repo = repo_at("/repo");
        assert_eq!(repo.classify(Path::new("/repo/src/main.rs")), Change::Tree);
        for git_state in [
            "/repo/.git/HEAD",
            "/repo/.git/index",
            "/repo/.git/logs/HEAD",
            "/repo/.git/packed-refs",
        ] {
            assert_eq!(
                repo.classify(Path::new(git_state)),
                Change::Git,
                "{git_state}"
            );
        }
    }

    #[test]
    fn toimija_git_footprint_is_ignored_without_hiding_git_state() {
        assert!(is_ignored_at("/repo", "/repo/.git/objects/ab/cdef"));
        assert!(is_ignored_at(
            "/repo",
            "/repo/.git/toimija-snapshot-123.index"
        ));
        assert!(is_ignored_at(
            "/repo",
            "/repo/.git/refs/toimija/snapshots/demo"
        ));
    }

    #[test]
    fn arming_surfaces_a_failure_for_an_unwatchable_path() {
        // A non-existent path cannot be watched; `start` must report it rather
        // than a background loop swallowing it.
        let err = match arm(&PathBuf::from("/does/not/exist/toimija-watch")) {
            Ok(_) => panic!("arming should fail for an unwatchable path"),
            Err(error) => error,
        };
        assert!(format!("{err:#}").contains("watch"));
    }

    /// What arms when native cannot: Git polling on Linux, never a walk of
    /// the whole tree; notify's polling elsewhere.
    const FALLBACK: &str = if cfg!(target_os = "linux") {
        "git polling"
    } else {
        "poll fallback"
    };

    #[test]
    fn native_probe_timeout_arms_the_fallback_and_says_so() {
        let temp = tempfile::tempdir().unwrap();
        let armed = start_with_probe(temp.path(), |repo_root, rx, poll, backend| {
            if poll.is_none() {
                Ok(false)
            } else {
                prove_ready(repo_root, rx, poll, backend)
            }
        })
        .unwrap();

        assert_eq!(armed.backend.label(), FALLBACK);
    }

    #[test]
    fn native_arm_failure_arms_the_fallback_instead_of_aborting() {
        // The inotify watch limit (ENOSPC) fails the native *arm*, not the
        // probe. Startup must degrade to polling, not abort.
        let temp = tempfile::tempdir().unwrap();
        let armed = start_from_native(
            Err(anyhow::anyhow!("injected: inotify watch limit reached")),
            temp.path(),
            prove_ready,
        )
        .unwrap();

        assert_eq!(armed.backend.label(), FALLBACK);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_arms_inotify_when_it_proves_ready() {
        let temp = tempfile::tempdir().unwrap();

        let armed = arm(temp.path()).unwrap();

        assert_eq!(armed.backend.label(), "native");
    }

    #[cfg(unix)]
    #[test]
    fn probe_accepts_the_symlink_resolved_path_native_backends_report() {
        // FSEvents reports /private/tmp/... for a repo rooted at /tmp/...;
        // the probe must prove ready from the canonical form, or native is
        // silently unreachable on any symlinked checkout path.
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let resolved = fs::canonicalize(&real)
            .unwrap()
            .join(format!("{PROBE_PREFIX}{}-native", std::process::id()));
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(Event::new(notify::EventKind::Create(
            notify::event::CreateKind::File,
        ))
        .add_path(resolved)))
            .unwrap();

        let ready = prove_ready(&link, &rx, None, "native").unwrap();

        assert!(ready);
    }

    #[test]
    fn probe_wait_keeps_one_total_deadline_under_steady_unrelated_churn() {
        // A per-receive timeout would restart on every event; unrelated churn
        // must not stall startup past the fallback window.
        let (tx, rx) = mpsc::channel();
        let feeder = std::thread::spawn(move || {
            for _ in 0..15 {
                let _ = tx.send(Ok(Event::new(notify::EventKind::Modify(
                    notify::event::ModifyKind::Any,
                ))
                .add_path(PathBuf::from("/unrelated"))));
                std::thread::sleep(Duration::from_millis(30));
            }
        });

        let started = Instant::now();
        let ready = wait_for_probe(
            &rx,
            None,
            Path::new("/probe"),
            Path::new("/probe"),
            Duration::from_millis(100),
        )
        .unwrap();
        let elapsed = started.elapsed();
        feeder.join().unwrap();

        assert!(!ready);
        assert!(
            elapsed < Duration::from_millis(300),
            "deadline extended by unrelated events: {elapsed:?}"
        );
    }

    #[test]
    fn errors_scoped_to_ignored_dirs_stay_quiet_but_others_surface() {
        let repo = repo_at("/repo");
        let build_churn = notify::Error::generic("walkdir error scanning")
            .add_path(PathBuf::from("/repo/_build/lib/vartio_core/priv"));
        assert!(repo.error_is_ignorable(&build_churn));

        // A pathless error says nothing about where it happened.
        let pathless = notify::Error::generic("backend failure");
        assert!(!repo.error_is_ignorable(&pathless));

        // One relevant path makes the whole error relevant.
        let mixed = notify::Error::generic("walkdir error scanning")
            .add_path(PathBuf::from("/repo/target/debug/x"))
            .add_path(PathBuf::from("/repo/src/main.rs"));
        assert!(!repo.error_is_ignorable(&mixed));
    }

    #[test]
    fn poll_scan_not_found_races_stay_quiet_but_real_io_errors_surface() {
        let temp = tempfile::tempdir().unwrap();
        let repo_root = temp.path();
        let repo = Repository::at(repo_root, repo_root.join(".git"), repo_root.join(".git"));
        let vanished_during_scan =
            notify::Error::io(std::io::Error::from(std::io::ErrorKind::NotFound))
                .add_path(repo_root.join(".git/objects/tmp_obj_abcd"));
        assert!(repo.error_is_ignorable(&vanished_during_scan));

        let pathless_not_found =
            notify::Error::io(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert!(!repo.error_is_ignorable(&pathless_not_found));

        let permission_denied =
            notify::Error::io(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
                .add_path(repo_root.join("private"));
        assert!(!repo.error_is_ignorable(&permission_denied));

        // Backends may canonicalize or otherwise alias the watched path. Root
        // presence, not lexical path equality, decides whether freshness is lost.
        let missing_repo_root = repo_root.join("missing");
        let missing = Repository::at(
            &missing_repo_root,
            missing_repo_root.join(".git"),
            missing_repo_root.join(".git"),
        );
        let missing_root_error =
            notify::Error::io(std::io::Error::from(std::io::ErrorKind::NotFound))
                .add_path(repo_root.join("different-spelling"));
        assert!(!missing.error_is_ignorable(&missing_root_error));
    }
    fn event(kind: notify::EventKind, path: PathBuf) -> notify::Result<Event> {
        Ok(Event::new(kind).add_path(path))
    }

    #[test]
    fn the_stamp_sees_a_detach_in_a_reftable_linked_worktree() {
        // In a reftable repository a linked worktree keeps its HEAD in its own
        // `reftable/`, beside no HEAD file that moves.
        if !git_usable() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        if !fixture_git(&base, &["init", "-q", "--ref-format=reftable", "probe"])
            .status
            .success()
        {
            return; // git older than 2.45: no reftable
        }
        let primary = base.join("primary");
        fs::create_dir(&primary).unwrap();
        git(
            &primary,
            &["init", "-q", "-b", "main", "--ref-format=reftable"],
        );
        git(
            &primary,
            &["config", "user.email", "toimija@example.invalid"],
        );
        git(&primary, &["config", "user.name", "Toimija Test"]);
        git(&primary, &["config", "commit.gpgsign", "false"]);
        fs::write(primary.join("a.txt"), "x\n").unwrap();
        git(&primary, &["add", "."]);
        git(&primary, &["commit", "-q", "-m", "init"]);
        let linked = base.join("linked");
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature",
                linked.to_str().unwrap(),
            ],
        );
        let repo = Repository::discover(&ProcessCommandRunner, &linked).unwrap();
        let before = repo.stamp();

        git(&linked, &["switch", "-q", "--detach"]);

        assert_ne!(
            repo.stamp().refs,
            before.refs,
            "the detach is in the worktree's reftable"
        );
    }

    #[test]
    fn a_root_build_dir_event_refreshes_only_when_git_status_shows_a_change() {
        // Untracked build output costs one `git status` and no refresh. A
        // tracked file under a build directory's name is never ignored.
        if !git_usable() {
            return;
        }
        let (temp, root) = repo_with_commit();
        fs::create_dir_all(root.join("deps")).unwrap();
        fs::write(root.join("deps/vendored.ex"), "v1\n").unwrap();
        fs::write(root.join(".gitignore"), "/target/\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "vendor"]);
        let runner = Traced::new(temp.path());

        let (session, events) = armed(&runner, &root, false);
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("target/debug/out"), "o\n").unwrap();
        let create = notify::EventKind::Create(notify::event::CreateKind::File);
        events
            .unwrap()
            .send(event(create, root.join("target/debug/out")))
            .unwrap();
        let before = runner.starts();
        assert_eq!(run_ticks(session, &runner, 2, FAST), 0, "build output");
        assert_eq!(runner.starts() - before, 1, "one `git status`");

        let (session, events) = armed(&runner, &root, false);
        fs::write(root.join("deps/vendored.ex"), "v2\n").unwrap();
        let modify = notify::EventKind::Modify(notify::event::ModifyKind::Any);
        events
            .unwrap()
            .send(event(modify, root.join("deps/vendored.ex")))
            .unwrap();
        assert_eq!(run_ticks(session, &runner, 2, FAST), 1, "a tracked file");
    }

    #[test]
    fn read_only_access_events_are_dropped_but_a_closed_write_is_kept() {
        // Inotify reports every open and close: the refresh's own reads would
        // re-trigger it, and the debounce would never go quiet.
        use notify::event::{AccessKind, AccessMode};
        for read in [
            AccessKind::Open(AccessMode::Read),
            AccessKind::Open(AccessMode::Any),
            AccessKind::Close(AccessMode::Read),
            AccessKind::Read,
            AccessKind::Any,
        ] {
            assert!(reads_only(&notify::EventKind::Access(read)), "{read:?}");
        }
        assert!(!reads_only(&notify::EventKind::Access(AccessKind::Close(
            AccessMode::Write
        ))));
        assert!(!reads_only(&notify::EventKind::Modify(
            notify::event::ModifyKind::Any
        )));
    }

    #[test]
    fn a_failed_refresh_backs_off_from_half_a_second_to_the_safety_net() {
        let mut delay = None;
        let mut delays = Vec::new();
        for _ in 0..10 {
            let next = next_retry_delay(delay, Timing::LIVE);
            delays.push(next.as_millis());
            delay = Some(next);
        }
        assert_eq!(
            delays,
            [
                500, 1000, 2000, 4000, 8000, 16000, 32000, 60000, 60000, 60000
            ]
        );
    }

    #[test]
    fn a_failing_refresh_is_retried_with_a_doubling_delay() {
        // The loop's clock is the fabricated one: it advances 5 ms a tick, so
        // the retries land on exact ticks however long each git call takes.
        if !git_usable() {
            return;
        }
        let (temp, root) = repo_with_commit();
        let runner = Traced::new(temp.path());
        let (session, _events) = armed(&runner, &root, false);
        // An unparseable configuration: every refresh fails until it is fixed.
        fs::create_dir_all(root.join(".toimija")).unwrap();
        fs::write(root.join(crate::config::CONFIG_PATH), "repo: [\n").unwrap();
        let timing = Timing {
            tick: Duration::from_millis(1),
            state_poll: Duration::ZERO,
            safety_net: Duration::from_millis(160),
            retry_first: Duration::from_millis(10),
        };
        let stop = AtomicBool::new(false);
        let tick = Cell::new(0u32);
        let ticked = Ticked {
            inner: &runner,
            tick: &tick,
            attempts: RefCell::new(Vec::new()),
        };
        let mono = Instant::now();

        session
            .run_with(
                &ticked,
                &plan(),
                &stop,
                || {},
                || {
                    let now = tick.get();
                    if now == 100 {
                        stop.store(true, Ordering::Relaxed);
                    }
                    tick.set(now + 1);
                    let elapsed = Duration::from_millis(5 * u64::from(now));
                    ClockPair {
                        wall: UNIX_EPOCH + elapsed,
                        mono: mono + elapsed,
                    }
                },
                timing,
            )
            .unwrap();

        // Tick 1 sees the moved configuration and fails at 5 ms. Retries wait
        // 10, 20, 40, 80, then 160 ms — the safety net's interval — twice.
        assert_eq!(*ticked.attempts.borrow(), [1, 3, 7, 15, 31, 63, 95]);
    }

    /// Records the loop's tick at each refresh attempt.
    struct Ticked<'a> {
        inner: &'a Traced,
        tick: &'a Cell<u32>,
        attempts: RefCell<Vec<u32>>,
    }

    impl CommandRunner for Ticked<'_> {
        fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            self.run_with_env(cwd, program, args, &[])
        }

        fn run_with_env(
            &self,
            cwd: &Path,
            program: &str,
            args: &[&str],
            env: &[(&str, &OsStr)],
        ) -> std::io::Result<CommandOutput> {
            if args == READS_THE_REFS {
                // The clock has been read for the tick that is running.
                self.attempts.borrow_mut().push(self.tick.get() - 1);
            }
            self.inner.run_with_env(cwd, program, args, env)
        }
    }
}
