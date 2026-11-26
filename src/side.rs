//! `toimija side`: the project view kept on screen in a narrow pane beside
//! the terminal where the work is done.
//!
//! It is an ordinary foreground command that redraws in place, so it works
//! in any split of any terminal and in a multiplexer pane. It lives as long
//! as its pane: no daemon, and nothing is written. It captures no mouse, so
//! text can still be selected.
//!
//! An idle tick starts no process. Each tick it reads a few file times for
//! each repository shown; git and sykli are asked again only for a
//! repository whose files moved, on `r`, and on a slow timer for uncommitted
//! edits, which touch no file git keeps. The other tools are asked on a
//! timer of their own, off the thread that draws: vessa for the agent
//! sessions of the last day and ohjaa for its summary.

use std::collections::BTreeMap;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result};

use crate::command::CommandRunner;
use crate::project::{self, Asked, View};
use crate::workspace;

mod follow;
mod look;
mod tools;

/// The width asked for when this command opens its own pane, in columns.
pub const PANE_COLUMNS: u16 = 38;
/// The title the view gives its pane, by which `--open` finds it again.
pub const PANE_TITLE: &str = "toimija side";
const TICK: Duration = Duration::from_millis(1500);
/// How long a frame stands while something on screen is moving.
const FRAME: Duration = Duration::from_millis(160);
/// The mark that turns while something is being asked.
const TURNING: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// How often uncommitted edits are looked for where a session is at work.
const BUSY_RECHECK: Duration = Duration::from_secs(20);
/// And where nobody is.
const QUIET_RECHECK: Duration = Duration::from_secs(180);
/// How often vessa and ohjaa are asked again.
const TOOLS_RECHECK: Duration = Duration::from_secs(120);

/// The file times that move when a repository's commits, index, branch or
/// receipts do. Equal fingerprints mean git and sykli need not be asked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fingerprint(Vec<Option<(SystemTime, u64)>>);

pub fn fingerprint(dir: &Path) -> Fingerprint {
    let git_dir = git_dir(dir);
    let stamp = |path: PathBuf| {
        std::fs::metadata(path)
            .ok()
            .and_then(|found| Some((found.modified().ok()?, found.len())))
    };
    Fingerprint(vec![
        stamp(git_dir.join("HEAD")),
        stamp(git_dir.join("index")),
        stamp(git_dir.join("logs/HEAD")),
        stamp(git_dir.join("packed-refs")),
        stamp(git_dir.join("FETCH_HEAD")),
        stamp(git_dir.join("sykli/receipts")),
        stamp(dir.join(".sykli/receipts")),
    ])
}

/// The git directory of the working copy at `dir`: `.git`, or where a
/// `.git` file says it is.
fn git_dir(dir: &Path) -> PathBuf {
    let dot_git = dir.join(".git");
    match std::fs::read_to_string(&dot_git) {
        Ok(text) => match text.trim().strip_prefix("gitdir:") {
            Some(path) => dir.join(path.trim()),
            None => dot_git,
        },
        Err(_) => dot_git,
    }
}

struct Kept {
    fingerprint: Fingerprint,
    asked: Asked,
    at: Instant,
}

/// What was last learned of each repository, and when.
#[derive(Default)]
pub struct Memory {
    kept: BTreeMap<String, Kept>,
    /// The file times the last view was built from.
    seen: Option<Vec<Fingerprint>>,
}

/// The file times a view of these repositories rests on: the workspace
/// file, and for each repository its git files, its sessions and its
/// handover.
fn stamps(root: &Path, names: &[String]) -> Vec<Fingerprint> {
    let stamp = |path: PathBuf| {
        std::fs::metadata(path)
            .ok()
            .and_then(|found| Some((found.modified().ok()?, found.len())))
    };
    let mut all = vec![Fingerprint(vec![stamp(
        root.join(workspace::WORKSPACE_PATH),
    )])];
    for name in names {
        let dir = root.join(name);
        all.push(fingerprint(&dir));
        all.push(Fingerprint(vec![
            stamp(dir.join(".toimija/sessions")),
            stamp(dir.join(".toimija/handover.json")),
        ]));
    }
    all
}

impl Memory {
    /// Forget everything: the next view asks every repository again.
    pub fn clear(&mut self) {
        self.kept.clear();
        self.seen = None;
    }

    /// Whether any file the last view rests on has moved since. A handful
    /// of file times for each repository shown; no file is read and no
    /// process is started.
    pub fn moved(&self, root: &Path) -> bool {
        let names: Vec<String> = self.kept.keys().cloned().collect();
        self.seen.as_ref() != Some(&stamps(root, &names))
    }

    /// Build the view. `ask` is handed only the repositories whose files
    /// moved or whose answer is older than the recheck time allows; with
    /// nothing to ask it is not called, and no process is started.
    pub fn view(
        &mut self,
        runner: &dyn CommandRunner,
        root: &Path,
        only: Option<&str>,
        now: Instant,
        ask: &mut dyn FnMut(&[String]) -> BTreeMap<String, Asked>,
    ) -> Result<View> {
        let packet = workspace::build(runner, root)?;
        let mut stale = Vec::new();
        let mut prints = BTreeMap::new();
        for name in project::shown(&packet, root, only)? {
            let fresh = fingerprint(&root.join(&name));
            let busy = packet
                .repositories
                .iter()
                .any(|repository| repository.name == name && !repository.sessions.is_empty());
            let recheck = if busy { BUSY_RECHECK } else { QUIET_RECHECK };
            let current = self.kept.get(&name).is_some_and(|known| {
                known.fingerprint == fresh && now.duration_since(known.at) < recheck
            });
            if !current {
                stale.push(name.clone());
            }
            prints.insert(name, fresh);
        }
        if !stale.is_empty() {
            for (name, asked) in ask(&stale) {
                let fingerprint = prints.get(&name).cloned().unwrap_or_default();
                self.kept.insert(
                    name,
                    Kept {
                        fingerprint,
                        asked,
                        at: now,
                    },
                );
            }
        }
        // Only what is shown is remembered, so `moved` watches just that.
        self.kept.retain(|name, _| prints.contains_key(name));
        let names: Vec<String> = self.kept.keys().cloned().collect();
        self.seen = Some(stamps(root, &names));
        let kept = &self.kept;
        project::build_from(&packet, root, only, &mut |name, _| {
            kept.get(name)
                .map(|known| known.asked.clone())
                .unwrap_or_default()
        })
    }
}

pub struct Options {
    pub project: Option<String>,
    pub once: bool,
    pub width: Option<usize>,
    /// Narrow this pane to [`PANE_COLUMNS`] first, where the terminal can be
    /// asked to. Set by `--open`.
    pub fit: bool,
    /// The shell the pane was opened from: the view follows the repository
    /// that shell is working in. Set by `--open`.
    pub follow: Option<u32>,
}

/// Print the view once, or keep it on screen when standard output is a
/// terminal. Exit code 4 when part of it could not be read.
pub fn run(runner: &dyn CommandRunner, root: &Path, options: Options) -> Result<i32> {
    if options.once || !std::io::stdout().is_terminal() {
        let view = project::build(runner, root, options.project.as_deref())?;
        let width = options.width.unwrap_or(usize::from(PANE_COLUMNS));
        print!("{}", project::render_text(&view, width));
        return Ok(if view.warnings.is_empty() { 0 } else { 4 });
    }
    #[cfg(unix)]
    {
        live(runner, root, options)
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("the live view needs a Unix terminal; use --once")
    }
}

#[cfg(unix)]
mod screen {
    use std::io::Write as _;
    use std::sync::OnceLock;

    static SAVED: OnceLock<libc::termios> = OnceLock::new();

    /// The terminal as it was, put back on drop and on panic.
    pub struct Screen;

    pub fn enter() -> std::io::Result<Screen> {
        // SAFETY: tcgetattr and tcsetattr are given a valid descriptor and a
        // termios this function owns.
        unsafe {
            let mut mode: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut mode) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let _ = SAVED.set(mode);
            // Keys arrive one at a time and are not echoed. Signals stay on,
            // so ctrl-c still ends the command.
            mode.c_lflag &= !(libc::ICANON | libc::ECHO);
            mode.c_cc[libc::VMIN] = 0;
            mode.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &mode) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        let before = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            before(info);
        }));
        // The alternate screen, so nothing is left in the scrollback; no cursor.
        let mut out = std::io::stdout().lock();
        out.write_all(b"\x1b[?1049h\x1b[?25l\x1b]2;toimija side\x07")?;
        out.flush()?;
        Ok(Screen)
    }

    pub fn restore() {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
        if let Some(mode) = SAVED.get() {
            // SAFETY: the termios is the one tcgetattr filled in.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, mode);
            }
        }
    }

    impl Drop for Screen {
        fn drop(&mut self) {
            restore();
        }
    }

    /// Columns and rows of the terminal, when it says.
    pub fn size() -> Option<(usize, usize)> {
        // SAFETY: TIOCGWINSZ fills the winsize it is given.
        unsafe {
            let mut size: libc::winsize = std::mem::zeroed();
            (libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) == 0
                && size.ws_col > 0
                && size.ws_row > 0)
                .then(|| (usize::from(size.ws_col), usize::from(size.ws_row)))
        }
    }

    /// Columns, and the width of the terminal in pixels when it says.
    #[cfg(target_os = "macos")]
    pub fn columns_and_pixels() -> Option<(usize, usize)> {
        // SAFETY: TIOCGWINSZ fills the winsize it is given.
        unsafe {
            let mut size: libc::winsize = std::mem::zeroed();
            (libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) == 0 && size.ws_col > 0)
                .then(|| (usize::from(size.ws_col), usize::from(size.ws_xpixel)))
        }
    }

    /// A key as the view understands it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Key {
        Up,
        Down,
        Back,
        Char(u8),
    }

    /// One key, waiting at most `wait`. An arrow arrives as three bytes; a
    /// lone escape means back.
    pub fn read_key(wait: std::time::Duration) -> Option<Key> {
        let soon = std::time::Duration::from_millis(15);
        match key(wait)? {
            0x1b => match key(soon) {
                Some(b'[' | b'O') => match key(soon) {
                    Some(b'A') => Some(Key::Up),
                    Some(b'B') => Some(Key::Down),
                    _ => None,
                },
                _ => Some(Key::Back),
            },
            0x7f | 0x08 => Some(Key::Back),
            byte => Some(Key::Char(byte)),
        }
    }

    /// Set once the terminal is gone: its other end was closed, and no
    /// key will ever come.
    static GONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Whether the terminal this view draws in has gone away. A terminal
    /// that closes a pane does not always signal the program in it, so
    /// this is how the view learns that it has no pane any more.
    pub fn gone() -> bool {
        GONE.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// What reading one byte gave.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Read {
        Byte(u8),
        Nothing,
        /// The other end is closed.
        Gone,
    }

    /// One byte from `fd`, waiting at most `wait`.
    pub fn read_from(fd: libc::c_int, wait: std::time::Duration) -> Read {
        let mut watched = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = i32::try_from(wait.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: one valid pollfd, and a one-byte buffer for read.
        unsafe {
            if libc::poll(&mut watched, 1, millis) <= 0 {
                return Read::Nothing;
            }
            if watched.revents & libc::POLLNVAL != 0 {
                return Read::Gone;
            }
            let mut byte = 0u8;
            match libc::read(fd, (&raw mut byte).cast(), 1) {
                1 => Read::Byte(byte),
                // Ready to read and nothing there is the end of input.
                0 => Read::Gone,
                _ => match std::io::Error::last_os_error().kind() {
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock => {
                        Read::Nothing
                    }
                    _ => Read::Gone,
                },
            }
        }
    }

    /// One key, waiting at most `wait`. `None` when none came.
    pub fn key(wait: std::time::Duration) -> Option<u8> {
        match read_from(libc::STDIN_FILENO, wait) {
            Read::Byte(byte) => Some(byte),
            Read::Nothing => None,
            Read::Gone => {
                GONE.store(true, std::sync::atomic::Ordering::Relaxed);
                None
            }
        }
    }
}

/// What the pane is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Showing {
    /// Where each repository of the projects stands.
    Projects,
    /// The branch graph of one repository.
    Graph(String),
    /// The declared relationships between the repositories shown.
    Relations,
    /// The agent sessions of the last day, as vessa read them.
    Agents,
}

impl Showing {
    /// Where this view stands in the row of views.
    fn number(&self) -> usize {
        match self {
            Showing::Projects => 0,
            Showing::Graph(_) => 1,
            Showing::Relations => 2,
            Showing::Agents => 3,
        }
    }
}

/// What a key asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Asks {
    Quit,
    /// Read everything again.
    Again,
    /// Draw again: something on screen changed.
    Draw,
    Nothing,
}

/// The state a key changes: what is shown, which project, and which
/// repository is picked.
struct Pane {
    showing: Showing,
    only: Option<String>,
    /// The projects, to step through with `p`.
    projects: Vec<String>,
    /// The repositories on screen, in order, and the one picked.
    repositories: Vec<String>,
    picked: usize,
}

impl Pane {
    fn picked(&self) -> Option<&str> {
        self.repositories.get(self.picked).map(String::as_str)
    }

    #[cfg(unix)]
    fn press(&mut self, key: screen::Key) -> Asks {
        use screen::Key;
        match (&self.showing, key) {
            (_, Key::Char(b'q')) => Asks::Quit,
            (_, Key::Char(b'r')) => Asks::Again,
            (_, Key::Char(b'1')) => {
                self.showing = Showing::Projects;
                Asks::Draw
            }
            (_, Key::Char(b'2')) => match self.picked() {
                Some(name) => {
                    self.showing = Showing::Graph(name.to_string());
                    Asks::Draw
                }
                None => Asks::Nothing,
            },
            (_, Key::Char(b'3')) => {
                self.showing = Showing::Relations;
                Asks::Draw
            }
            (_, Key::Char(b'4')) => {
                self.showing = Showing::Agents;
                Asks::Draw
            }
            (
                Showing::Graph(_) | Showing::Relations | Showing::Agents,
                Key::Back | Key::Char(b'p' | b'g' | b'h'),
            ) => {
                self.showing = Showing::Projects;
                Asks::Draw
            }
            (Showing::Projects, Key::Down | Key::Char(b'j')) => {
                if self.picked + 1 < self.repositories.len() {
                    self.picked += 1;
                }
                Asks::Draw
            }
            (Showing::Projects, Key::Up | Key::Char(b'k')) => {
                self.picked = self.picked.saturating_sub(1);
                Asks::Draw
            }
            (Showing::Projects, Key::Char(b'g' | b'\r' | b'\n' | b'l')) => match self.picked() {
                Some(name) => {
                    self.showing = Showing::Graph(name.to_string());
                    Asks::Draw
                }
                None => Asks::Nothing,
            },
            (Showing::Projects, Key::Char(b'p' | b'\t')) => {
                self.only = next_project(&self.projects, self.only.as_deref());
                self.picked = 0;
                Asks::Draw
            }
            _ => Asks::Nothing,
        }
    }
}

/// What was learned of one repository's branches: the graph or why it
/// could not be read, the commits sykli counts each branch ahead of the
/// base, and the working copies with the handover left in each.
struct Seen {
    graph: Result<crate::graph::Graph, String>,
    ahead: std::collections::HashMap<String, u64>,
    copies: Vec<follow::WorkingCopy>,
}

impl Seen {
    fn read(dir: &Path) -> Self {
        Self {
            graph: crate::graph::build(
                &crate::command::ProcessCommandRunner,
                dir,
                "origin/main",
                &crate::graph::sykli_proof,
            )
            .map_err(|error| format!("{error:#}")),
            ahead: ahead_by_branch(dir),
            copies: follow::working_copies(dir),
        }
    }
}

/// The commits each local branch is ahead of the base, from
/// `sykli status --json`. A sykli that is absent or fails leaves it empty:
/// no count is then shown, rather than a nought.
fn ahead_by_branch(dir: &Path) -> std::collections::HashMap<String, u64> {
    let Ok(output) = std::process::Command::new("sykli")
        .args(["status", "--json"])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Default::default();
    };
    ahead_from(&String::from_utf8_lossy(&output.stdout))
}

fn ahead_from(status: &str) -> std::collections::HashMap<String, u64> {
    let Ok(status) = serde_json::from_str::<serde_json::Value>(status) else {
        return Default::default();
    };
    status
        .get("branches")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|branch| {
            Some((
                branch.get("branch")?.as_str()?.to_string(),
                branch.get("ahead")?.as_u64()?,
            ))
        })
        .collect()
}

/// The graphs already asked for, by repository, and the files each rested on.
type Graphs = std::collections::HashMap<String, (Fingerprint, Seen)>;

/// A repository's branches being asked about on a thread of its own, so
/// the pane keeps drawing and taking keys meanwhile.
struct Asking {
    name: String,
    files: Fingerprint,
    got: Arc<Mutex<Option<Seen>>>,
}

impl Asking {
    fn start(name: &str, dir: PathBuf, files: Fingerprint) -> Self {
        let got = Arc::new(Mutex::new(None));
        let put = Arc::clone(&got);
        std::thread::spawn(move || {
            let seen = Seen::read(&dir);
            *put.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(seen);
        });
        Self {
            name: name.to_string(),
            files,
            got,
        }
    }

    fn answer(&self) -> Option<Seen> {
        self.got
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

/// Everything a frame is drawn from.
struct Scene<'a> {
    pane: &'a Pane,
    folder: &'a str,
    /// `None` until the repositories have been asked about once.
    view: Option<&'a Result<View, String>>,
    facts: Option<&'a tools::Facts>,
    graphs: &'a Graphs,
    /// The repository whose graph is being asked for now.
    asking: Option<&'a str>,
    root: &'a Path,
    here: Option<&'a follow::Here>,
    turning: Option<char>,
    pulse: u8,
    now: u64,
    width: usize,
    room: usize,
}

impl Scene<'_> {
    fn title(&self) -> String {
        match &self.pane.showing {
            Showing::Graph(name) => name.clone(),
            Showing::Projects | Showing::Relations | Showing::Agents => self
                .pane
                .only
                .clone()
                .unwrap_or_else(|| self.folder.to_string()),
        }
    }

    /// The repository whose graph this view shows, if it shows one.
    fn graph_of(&self) -> Option<String> {
        match &self.pane.showing {
            Showing::Projects => self.pane.picked().map(str::to_string),
            Showing::Graph(name) => Some(name.clone()),
            Showing::Relations | Showing::Agents => None,
        }
    }

    fn repositories(&self) -> Vec<look::Text> {
        let told = |tone: look::Tone, text: String| {
            look::card(
                "REPOSITORIES",
                look::Text::new(),
                vec![look::Text::of(tone, text)],
                self.width,
            )
        };
        match self.view {
            None => told(look::Tone::Dim, "asking git and sykli…".to_string()),
            Some(Ok(view)) => look::repositories(view, self.pane.picked(), self.width),
            Some(Err(why)) => told(look::Tone::Warn, format!("not read: {why}")),
        }
    }

    /// The agent sessions of the last day that work in one of these
    /// working copies, by the branch checked out there.
    fn agents_on(&self, copies: &[follow::WorkingCopy]) -> Vec<look::OnBranch> {
        let Some(Ok(agents)) = self.facts.map(|facts| &facts.agents) else {
            return Vec::new();
        };
        let mut sessions: Vec<&tools::Session> = agents.digest.sessions.iter().collect();
        sessions.sort_by(|a, b| b.last_at.cmp(&a.last_at));
        sessions
            .into_iter()
            .filter_map(|session| {
                let cwd = session.cwd.as_deref()?;
                let branch = follow::copy_of(copies, Path::new(cwd))?.branch.clone()?;
                Some(look::OnBranch {
                    branch,
                    agent: session.agent.clone(),
                    age: tools::age(session.last_at.as_deref(), self.now),
                    written: tools::tokens(session.output_tokens),
                })
            })
            .collect()
    }

    /// The lines under the row of views, and the keys of this view.
    fn body(&self) -> (Vec<look::Text>, look::Text) {
        let facts = self.facts;
        let width = self.width;
        let mut body: Vec<look::Text> = Vec::new();
        // What goes under the graph, which takes the room between.
        let mut below: Vec<look::Text> = Vec::new();
        let keys = match &self.pane.showing {
            Showing::Projects => {
                body.extend(look::tiles(facts, self.pulse, width));
                body.extend(self.repositories());
                below.extend(look::agents(facts, self.now, 2, false, self.pulse, width));
                look::keys(&[
                    ("jk", "pick"),
                    ("p", "project"),
                    ("r", "read"),
                    ("q", "quit"),
                ])
            }
            Showing::Relations => {
                match self.view {
                    Some(Ok(view)) => body.extend(look::links(view, width)),
                    _ => body.extend(self.repositories()),
                }
                look::keys(&[
                    ("1", "back"),
                    ("p", "project"),
                    ("r", "read"),
                    ("q", "quit"),
                ])
            }
            Showing::Agents => {
                body.extend(look::tiles(facts, self.pulse, width));
                body.extend(look::totals(facts, width));
                body.extend(look::agents(
                    facts,
                    self.now,
                    usize::MAX,
                    false,
                    self.pulse,
                    width,
                ));
                body.extend(look::knowledge(facts, true, width));
                look::keys(&[("1", "back"), ("r", "read"), ("q", "quit")])
            }
            Showing::Graph(_) => look::keys(&[("1", "back"), ("r", "read"), ("q", "quit")]),
        };
        // The graph is on screen without a key. In the first view it is the
        // picked repository's and has the room between the repositories and
        // the agents; where that is too little the agents give way. View 2
        // gives it the whole pane.
        if let Some(name) = self.graph_of() {
            let room = self.room.saturating_sub(body.len() + 2);
            if room < below.len() + GRAPH_ROWS {
                below.clear();
            }
            let most = room.saturating_sub(below.len());
            if most >= 2 {
                let seen = self.graphs.get(&name).map(|(_, seen)| seen);
                let branches = match seen.map(|seen| &seen.graph) {
                    Some(Ok(graph)) => look::Branches::Read(graph),
                    Some(Err(why)) => look::Branches::NotRead(why),
                    None => look::Branches::Reading,
                };
                let none = std::collections::HashMap::new();
                // While the first answer is awaited the working copies are
                // read here, so a handover shows at once.
                let copies = match seen {
                    Some(seen) => seen.copies.clone(),
                    None => follow::working_copies(&self.root.join(&name)),
                };
                let agents = self.agents_on(&copies);
                let around = look::Around {
                    // The branch the shell has checked out, when this is
                    // its repository.
                    here: self
                        .here
                        .filter(|here| here.repository == name)
                        .and_then(|here| here.branch.as_deref()),
                    turning: self.turning.filter(|_| self.asking == Some(name.as_str())),
                    ahead: seen.map_or(&none, |seen| &seen.ahead),
                    copies: &copies,
                    agents: &agents,
                    pulse: self.pulse,
                    now: self.now,
                };
                body.extend(look::graph(&name, branches, &around, most, width));
            }
        }
        body.extend(below);
        (body, keys)
    }
}

/// What vessa and ohjaa last said, asked again on a timer by a thread of
/// its own: vessa takes seconds, and the pane must not wait for it.
#[derive(Default)]
struct Tools {
    facts: Arc<Mutex<Option<tools::Facts>>>,
    busy: Arc<AtomicBool>,
    /// Set when an answer came that is not on screen yet.
    arrived: Arc<AtomicBool>,
    asked: Option<Instant>,
}

impl Tools {
    fn due(&self) -> bool {
        self.asked.is_none_or(|at| at.elapsed() >= TOOLS_RECHECK)
    }

    fn ask(&mut self, root: &Path) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.asked = Some(Instant::now());
        let (facts, busy, arrived) = (
            Arc::clone(&self.facts),
            Arc::clone(&self.busy),
            Arc::clone(&self.arrived),
        );
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            let read = tools::Facts::read(&root);
            *facts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(read);
            busy.store(false, Ordering::SeqCst);
            arrived.store(true, Ordering::SeqCst);
        });
    }

    fn facts(&self) -> Option<tools::Facts> {
        self.facts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[cfg(unix)]
fn live(runner: &dyn CommandRunner, root: &Path, options: Options) -> Result<i32> {
    let stop = Arc::new(AtomicBool::new(false));
    let resized = Arc::new(AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }
    signal_hook::flag::register(signal_hook::consts::SIGWINCH, Arc::clone(&resized))?;

    let mut pane = Pane {
        showing: Showing::Projects,
        only: options.project.clone(),
        projects: workspace::load(root)?
            .projects
            .into_iter()
            .map(|project| project.name)
            .collect(),
        repositories: Vec::new(),
        picked: 0,
    };
    let mut memory = Memory::default();
    let mut graphs: Graphs = Graphs::new();
    let mut asking: Option<Asking> = None;
    let mut tools = Tools::default();
    let _screen = screen::enter().context("the terminal could not be prepared")?;
    if options.fit {
        fit_pane();
    }
    let palette = look::Palette::detect();
    let folder = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    // What was last learned of the repositories, and when they were asked.
    let mut shown: Option<Result<View, String>> = None;
    let mut asked: Option<Instant> = None;
    // When the repositories' files were last looked at for movement.
    let mut looked: Option<Instant> = None;
    // Where the shell beside the pane works, and the directory that says so.
    let mut beside: Option<PathBuf> = None;
    let mut here: Option<follow::Here> = None;
    // The frame on which news last arrived: a light then crosses the name.
    let mut news: Option<u64> = None;
    let mut frame: u64 = 0;
    let mut stale = true;

    while !stop.load(Ordering::Relaxed) && !screen::gone() {
        frame += 1;
        let mut redraw = false;
        if tools.due() {
            tools.ask(root);
        }
        if tools.arrived.swap(false, Ordering::SeqCst) {
            news = Some(frame);
            redraw = true;
        }
        if resized.swap(false, Ordering::Relaxed) {
            redraw = true;
        }
        if let Some(answer) = asking.as_ref().and_then(Asking::answer) {
            if let Some(done) = asking.take() {
                graphs.insert(done.name, (done.files, answer));
            }
            news = Some(frame);
            redraw = true;
        }
        // The shell moved to another directory: show the repository it is in.
        let mut moved_to: Option<String> = None;
        if let Some(shell) = options.follow {
            let now = follow::cwd_of(shell);
            if now != beside {
                here = now.as_deref().and_then(|dir| follow::here(root, dir));
                moved_to = here.as_ref().map(|here| here.repository.clone());
                beside = now;
                redraw = true;
            }
        }
        // The repositories are asked about when their files moved, and on a
        // slow timer for uncommitted edits, which move no file git keeps.
        if looked.is_none_or(|at| at.elapsed() >= TICK) {
            looked = Some(Instant::now());
            if asked.is_none_or(|at| at.elapsed() >= BUSY_RECHECK) || memory.moved(root) {
                stale = true;
            }
        }
        let facts = tools.facts();
        let lively = facts
            .as_ref()
            .and_then(|facts| facts.agents.as_ref().ok())
            .is_some_and(|agents| !agents.digest.live.is_empty());
        let busy = asking.is_some() || tools.busy.load(Ordering::SeqCst);
        let lit = news.is_some_and(|at| frame - at < 8);
        let moving = lively || busy || lit;

        if !stale && !redraw {
            let wait = if moving { FRAME } else { TICK };
            match screen::read_key(wait).map(|key| pane.press(key)) {
                Some(Asks::Quit) => return Ok(0),
                Some(Asks::Again) => {
                    memory.clear();
                    graphs.clear();
                    tools.asked = None;
                    stale = true;
                    continue;
                }
                Some(Asks::Draw) => {}
                Some(Asks::Nothing) | None if !moving => continue,
                Some(Asks::Nothing) | None => {}
            }
        }

        let (columns, rows) = screen::size().unwrap_or((usize::from(PANE_COLUMNS), 40));
        let width = options.width.unwrap_or(columns).min(columns);
        // A turn of the mark each frame, and a slow swell of the live marks.
        let turning = busy.then(|| TURNING[(frame % 10) as usize]);
        let swell = frame % 14;
        let pulse = if swell < 8 { swell } else { 14 - swell } as u8;
        let sweep = news
            .map(|at| (frame - at) as usize)
            .filter(|letter| *letter < 7);
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or(0);
        macro_rules! scene {
            () => {
                Scene {
                    pane: &pane,
                    folder: &folder,
                    view: shown.as_ref(),
                    facts: facts.as_ref(),
                    graphs: &graphs,
                    asking: asking.as_ref().map(|asking| asking.name.as_str()),
                    root,
                    here: here.as_ref(),
                    turning,
                    pulse,
                    now,
                    width,
                    room: rows.saturating_sub(FRAME_ROWS),
                }
            };
        }

        if stale {
            if shown.is_none() {
                // The first answer takes seconds: show the frame it will fill.
                let scene = scene!();
                let (body, keys) = scene.body();
                draw(
                    &scene.title(),
                    pane.showing.number(),
                    &body,
                    &keys,
                    Some(TURNING[0]),
                    None,
                    width,
                    rows,
                    palette,
                )?;
            }
            let view = memory.view(
                runner,
                root,
                pane.only.as_deref(),
                Instant::now(),
                &mut |names| project::ask_many(root, names),
            );
            if let Ok(view) = &view {
                let mut names: Vec<String> = Vec::new();
                for row in view.projects.iter().flat_map(|p| &p.repositories) {
                    if row.present && !names.contains(&row.name) {
                        names.push(row.name.clone());
                    }
                }
                pane.repositories = names;
                pane.picked = pane.picked.min(pane.repositories.len().saturating_sub(1));
            }
            shown = Some(view.map_err(|error| format!("{error:#}")));
            asked = Some(Instant::now());
            stale = false;
            // The shell's repository is picked once the names are known.
            if moved_to.is_none() && frame == 1 {
                moved_to = here.as_ref().map(|here| here.repository.clone());
            }
        }
        if let Some(at) =
            moved_to.and_then(|name| pane.repositories.iter().position(|n| *n == name))
        {
            pane.picked = at;
        }

        // A graph whose repository's files moved is asked for again, one at
        // a time, on a thread of its own; the one on screen stays meanwhile.
        let scene = scene!();
        if let Some(name) = scene.graph_of() {
            let dir = root.join(&name);
            let files = fingerprint(&dir);
            let fresh = graphs.get(&name).is_some_and(|(at, _)| *at == files);
            if !fresh && asking.is_none() {
                asking = Some(Asking::start(&name, dir, files));
            }
        }
        let scene = scene!();
        let (body, keys) = scene.body();
        draw(
            &scene.title(),
            pane.showing.number(),
            &body,
            &keys,
            turning.or(asking.as_ref().map(|_| TURNING[(frame % 10) as usize])),
            sweep,
            width,
            rows,
            palette,
        )?;
    }
    Ok(0)
}

/// The rows of a frame that are not its body: the name, the views and a
/// blank above; what the marks mean and the keys below.
const FRAME_ROWS: usize = 5;
/// The least rows the graph is given before the agents under it give way.
const GRAPH_ROWS: usize = 10;

/// Narrow this pane to [`PANE_COLUMNS`] by asking Ghostty to move the
/// divider. Ghostty sizes splits in pixels and does not say which way a
/// direction moves this pane, so each step is measured and the next one
/// corrected: a wrong direction is turned round, a wrong scale is rescaled.
/// Any failure leaves the pane as it is.
#[cfg(target_os = "macos")]
fn fit_pane() {
    let Some(tty) = own_tty() else { return };
    if std::env::var("TERM_PROGRAM").as_deref() != Ok("ghostty") {
        return;
    }
    let want = usize::from(PANE_COLUMNS);
    let mut narrower = "right";
    let mut wider = "left";
    let mut scale = 1.0_f64;
    for _ in 0..5 {
        let Some((columns, pixels)) = screen::columns_and_pixels() else {
            return;
        };
        if columns.abs_diff(want) <= 1 || pixels == 0 {
            return;
        }
        let cell = pixels as f64 / columns as f64;
        let amount = (columns.abs_diff(want) as f64 * cell * scale)
            .round()
            .max(1.0) as u64;
        let direction = if columns > want { narrower } else { wider };
        let script = format!(
            "tell application \"Ghostty\" to perform action {} on (first terminal whose tty is {})",
            applescript_string(&format!("resize_split:{direction},{amount}")),
            applescript_string(&tty),
        );
        let asked = std::process::Command::new("osascript")
            .args(["-e", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if !asked.is_ok_and(|status| status.success()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
        let Some((after, _)) = screen::columns_and_pixels() else {
            return;
        };
        if after == columns {
            return;
        }
        let meant_narrower = columns > want;
        if (after < columns) != meant_narrower {
            std::mem::swap(&mut narrower, &mut wider);
        } else {
            let moved = columns.abs_diff(after) as f64;
            let asked = columns.abs_diff(want) as f64;
            scale = (scale * asked / moved).clamp(0.2, 5.0);
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn fit_pane() {}

/// After every project, each in turn, then every project again.
fn next_project(names: &[String], now: Option<&str>) -> Option<String> {
    match now {
        None => names.first().cloned(),
        Some(current) => names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(current))
            .and_then(|at| names.get(at + 1))
            .cloned(),
    }
}

/// One frame, written once, inside the terminal's synchronized-output marks
/// so a half-drawn frame is never shown.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn draw(
    title: &str,
    on: usize,
    body: &[look::Text],
    keys: &look::Text,
    turning: Option<char>,
    sweep: Option<usize>,
    width: usize,
    rows: usize,
    palette: look::Palette,
) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let clock = format!("{:02}:{:02} UTC", now % 86_400 / 3_600, now % 3_600 / 60);
    let room = rows.saturating_sub(FRAME_ROWS);

    let mut frame = String::from("\x1b[?2026h\x1b[H");
    let mut line = |text: look::Text| {
        frame.push_str(&text.fit(width).ansi(palette));
        frame.push_str("\x1b[K\r\n");
    };
    line(look::header(title, &clock, turning, sweep, width));
    line(look::tabs(on));
    line(look::Text::new());
    for text in body.iter().take(room) {
        line(text.clone());
    }
    frame.push_str("\x1b[J");
    // The last two rows: what the marks mean, and the keys.
    frame.push_str(&format!("\x1b[{};1H", rows.saturating_sub(1)));
    frame.push_str(&look::legend().fit(width).ansi(palette));
    frame.push_str(&format!("\x1b[K\x1b[{rows};1H"));
    frame.push_str(&keys.clone().fit(width).ansi(palette));
    frame.push_str("\x1b[K\x1b[?2026l");

    let mut out = std::io::stdout().lock();
    out.write_all(frame.as_bytes())?;
    out.flush()?;
    Ok(())
}

/// How `side --open` would open the pane here, as a program and its
/// arguments, or the reason it cannot.
pub fn opener(
    exe: &Path,
    root: &Path,
    project: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
    tty: Option<&str>,
    shell: Option<u32>,
) -> std::result::Result<(String, Vec<String>), String> {
    let mut side = vec![exe.display().to_string(), "side".to_string()];
    if let Some(project) = project {
        side.push("--project".to_string());
        side.push(project.to_string());
    }
    // What a person is told to run is the command as they would type it;
    // the pane this opens is also told which shell to follow.
    let typed = side
        .iter()
        .map(|word| shell_quote(word))
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(shell) = shell {
        side.push("--follow".to_string());
        side.push(shell.to_string());
    }
    let shell_line = side
        .iter()
        .map(|word| shell_quote(word))
        .collect::<Vec<_>>()
        .join(" ");
    if env("TMUX").is_some_and(|value| !value.is_empty()) {
        // A pane of this window titled as the view titles itself is the
        // view: close it. Otherwise open one, keeping focus where it is.
        let script = format!(
            "pane=$(tmux list-panes -F '#{{pane_id}} #{{pane_title}}' | sed -n 's/ {PANE_TITLE}$//p' | head -1)\n\
             if [ -n \"$pane\" ]; then tmux kill-pane -t \"$pane\"\n\
             else tmux split-window -h -d -l {PANE_COLUMNS} -c {} {}\nfi",
            shell_quote(&root.display().to_string()),
            shell_quote(&shell_line),
        );
        return Ok(("sh".to_string(), vec!["-c".to_string(), script]));
    }
    if cfg!(target_os = "macos") && env("TERM_PROGRAM").as_deref() == Some("ghostty") {
        // The pane goes beside the terminal this was typed in, found by its
        // device. Without one there is no telling which window is meant, and
        // the front one may be another: refuse rather than guess.
        let Some(tty) = tty else {
            return Err(format!(
                "this was not run from a terminal, so there is no telling which Ghostty \
                 window is meant. Run it in the terminal the pane should sit beside:\n  \
                 {typed} --open"
            ));
        };
        // In the tab this terminal is in, a pane titled as the view titles
        // itself is the view: close it. Otherwise open one beside this
        // terminal and give the focus back. Closing asks the view to quit
        // first, as its `q` key does: Ghostty can take a pane away without
        // ending the program in it.
        let script = format!(
            "tell application \"Ghostty\"\n\
             set here to first terminal whose tty is {tty}\n\
             set panes to {{}}\n\
             repeat with w in windows\n\
             repeat with t in tabs of w\n\
             if (tty of terminals of t) contains {tty} then\n\
             set panes to panes & (terminals of t whose name is {title})\n\
             end if\n\
             end repeat\n\
             end repeat\n\
             if (count of panes) > 0 then\n\
             repeat with pane in panes\n\
             try\n\
             input text \"q\" to pane\n\
             end try\n\
             end repeat\n\
             delay 0.4\n\
             repeat with pane in panes\n\
             try\n\
             close pane\n\
             end try\n\
             end repeat\n\
             else\n\
             set cfg to new surface configuration\n\
             set command of cfg to {command}\n\
             set initial working directory of cfg to {folder}\n\
             split here direction right with configuration cfg\n\
             focus here\n\
             end if\n\
             end tell",
            tty = applescript_string(tty),
            title = applescript_string(PANE_TITLE),
            command = applescript_string(&format!("{shell_line} --fit")),
            folder = applescript_string(&root.display().to_string()),
        );
        return Ok(("osascript".to_string(), vec!["-e".to_string(), script]));
    }
    Err(format!(
        "this terminal cannot be asked for a pane from here. Open a split yourself and run:\n  \
         {typed}\n\
         In Ghostty, a key can do it; add to its config:\n  \
         keybind = super+shift+b=new_split:right"
    ))
}

/// The device of the terminal this process was started in, when it has one.
#[cfg(unix)]
pub fn own_tty() -> Option<String> {
    [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO]
        .into_iter()
        .find_map(|descriptor| {
            // SAFETY: ttyname returns null or a C string that is copied at once.
            unsafe {
                let name = libc::ttyname(descriptor);
                (!name.is_null()).then(|| {
                    std::ffi::CStr::from_ptr(name)
                        .to_string_lossy()
                        .into_owned()
                })
            }
        })
}

#[cfg(not(unix))]
pub fn own_tty() -> Option<String> {
    None
}

/// `word` as one word of a POSIX shell line.
fn shell_quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// `text` as an AppleScript string literal.
fn applescript_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The claim the whole design rests on: while nothing moves, a tick asks
    /// git and sykli nothing.
    #[test]
    fn nothing_is_asked_again_until_a_repositorys_files_move() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join(".toimija")).unwrap();
        std::fs::write(root.join(workspace::WORKSPACE_PATH), "").unwrap();
        for name in ["one", "two"] {
            std::fs::create_dir_all(root.join(name).join(".git")).unwrap();
            std::fs::write(root.join(name).join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        }
        let runner = crate::command::ProcessCommandRunner;
        let mut asked: Vec<Vec<String>> = Vec::new();
        let mut memory = Memory::default();
        let start = Instant::now();
        let look = |memory: &mut Memory, asked: &mut Vec<Vec<String>>, at: Instant| {
            memory
                .view(&runner, &root, None, at, &mut |names| {
                    asked.push(names.to_vec());
                    names
                        .iter()
                        .map(|name| (name.clone(), Asked::default()))
                        .collect()
                })
                .unwrap()
        };

        look(&mut memory, &mut asked, start);
        assert_eq!(asked, [["one", "two"]]);

        // The same files a tick later: nothing moved, and nothing is asked.
        assert!(!memory.moved(&root));
        look(&mut memory, &mut asked, start + TICK);
        assert_eq!(asked.len(), 1);

        // One repository's index moves: that is seen, and only it is asked.
        std::fs::write(root.join("two/.git/index"), "x").unwrap();
        assert!(memory.moved(&root));
        look(&mut memory, &mut asked, start + TICK * 2);
        assert_eq!(asked.last().unwrap(), &["two"]);

        // Uncommitted edits touch no file git keeps, so a quiet repository
        // is still asked again after the slow timer.
        look(&mut memory, &mut asked, start + QUIET_RECHECK + TICK * 3);
        assert_eq!(asked.last().unwrap(), &["one", "two"]);

        // And `r` forgets everything.
        memory.clear();
        look(&mut memory, &mut asked, start + QUIET_RECHECK + TICK * 4);
        assert_eq!(asked.last().unwrap(), &["one", "two"]);
    }

    #[cfg(unix)]
    #[test]
    fn keys_pick_a_repository_open_its_graph_and_come_back() {
        use screen::Key;
        let mut pane = Pane {
            showing: Showing::Projects,
            only: None,
            projects: vec!["Tools".to_string()],
            repositories: vec!["one".to_string(), "two".to_string()],
            picked: 0,
        };
        assert_eq!(pane.press(Key::Down), Asks::Draw);
        assert_eq!(pane.press(Key::Char(b'j')), Asks::Draw);
        assert_eq!(pane.picked(), Some("two"), "it stops at the last one");
        assert_eq!(pane.press(Key::Char(b'g')), Asks::Draw);
        assert_eq!(pane.showing, Showing::Graph("two".to_string()));
        // In a graph the picking keys do nothing, and back returns.
        assert_eq!(pane.press(Key::Down), Asks::Nothing);
        assert_eq!(pane.press(Key::Back), Asks::Draw);
        assert_eq!(pane.showing, Showing::Projects);
        assert_eq!(pane.press(Key::Up), Asks::Draw);
        assert_eq!(pane.picked(), Some("one"));
        // `p` moves to the next project and starts at its first repository.
        pane.picked = 1;
        assert_eq!(pane.press(Key::Char(b'p')), Asks::Draw);
        assert_eq!((pane.only.as_deref(), pane.picked), (Some("Tools"), 0));
        assert_eq!(pane.press(Key::Char(b'q')), Asks::Quit);
        assert_eq!(pane.press(Key::Char(b'r')), Asks::Again);
    }

    #[test]
    fn p_steps_through_every_project_and_back_to_all() {
        let names = vec!["Platform".to_string(), "Tools".to_string()];
        assert_eq!(next_project(&names, None).as_deref(), Some("Platform"));
        assert_eq!(
            next_project(&names, Some("platform")).as_deref(),
            Some("Tools")
        );
        assert_eq!(next_project(&names, Some("Tools")), None);
        assert_eq!(next_project(&[], None), None);
    }

    #[test]
    fn the_pane_is_told_which_shell_to_follow_and_a_person_is_not() {
        let tmux = |name: &str| (name == "TMUX").then(|| "/tmp/tmux-1/default,1,0".to_string());
        let (_, args) = opener(
            Path::new("/bin/toimija"),
            Path::new("/work"),
            Some("One"),
            &tmux,
            None,
            Some(4242),
        )
        .unwrap();
        assert!(
            args[1].contains("'/bin/toimija side --project One --follow 4242'"),
            "{}",
            args[1]
        );
        // The command a person is told to type names no process.
        let said = opener(
            Path::new("/bin/toimija"),
            Path::new("/work"),
            None,
            &|_| None,
            None,
            Some(4242),
        )
        .unwrap_err();
        assert!(said.contains("/bin/toimija side\n"), "{said}");
        assert!(!said.contains("4242"), "{said}");
    }

    #[test]
    fn inside_tmux_open_is_a_toggle_that_keeps_focus() {
        let env = |name: &str| (name == "TMUX").then(|| "/tmp/tmux-1/default,1,0".to_string());
        let (program, args) = opener(
            Path::new("/bin/toimija"),
            Path::new("/work"),
            None,
            &env,
            None,
            None,
        )
        .unwrap();
        assert_eq!(program, "sh");
        // A pane titled as the view is closed; otherwise one is opened.
        assert!(args[1].contains("s/ toimija side$//p"), "{}", args[1]);
        assert!(
            args[1].contains("tmux kill-pane -t \"$pane\""),
            "{}",
            args[1]
        );
        assert!(
            args[1].contains("else tmux split-window -h -d -l 38 -c /work '/bin/toimija side'"),
            "{}",
            args[1]
        );
    }

    #[test]
    fn a_project_name_cannot_become_shell_or_applescript() {
        // One word for the shell, however it is written.
        assert_eq!(shell_quote("a'; rm -rf ~; \""), "'a'\\''; rm -rf ~; \"'");
        assert_eq!(shell_quote("SDLC"), "SDLC");
        assert_eq!(shell_quote(""), "''");
        // And one string for AppleScript.
        assert_eq!(applescript_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[cfg(unix)]
    #[test]
    fn a_terminal_that_went_away_is_known_and_a_quiet_one_is_not() {
        use screen::Read;
        let mut ends = [0 as libc::c_int; 2];
        // SAFETY: a two-element array for the two ends of a pipe.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        let (reading, writing) = (ends[0], ends[1]);
        // Other tests start programs meanwhile; one that inherited the
        // writing end would keep the pipe open after this test closed it.
        for end in ends {
            // SAFETY: setting a flag on a descriptor this test opened.
            unsafe { libc::fcntl(end, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        let soon = Duration::from_millis(10);
        assert_eq!(screen::read_from(reading, soon), Read::Nothing);
        // SAFETY: one byte from a live buffer into an open descriptor.
        assert_eq!(unsafe { libc::write(writing, b"q".as_ptr().cast(), 1) }, 1);
        assert_eq!(screen::read_from(reading, soon), Read::Byte(b'q'));
        // SAFETY: closing a descriptor this test opened.
        unsafe { libc::close(writing) };
        assert_eq!(screen::read_from(reading, soon), Read::Gone);
        // SAFETY: as above.
        unsafe { libc::close(reading) };
    }

    #[test]
    fn sykli_says_how_far_ahead_each_branch_is_and_silence_is_not_nought() {
        let status = r#"{"schema":"sykli-branch-status.v1","branches":[
            {"branch":"feat/one","ahead":3,"behind":0},
            {"branch":"main","ahead":0,"behind":2},
            {"branch":"odd"}]}"#;
        let ahead = ahead_from(status);
        assert_eq!(ahead.get("feat/one"), Some(&3));
        assert_eq!(ahead.get("main"), Some(&0));
        assert_eq!(ahead.get("odd"), None, "no count given is no count");
        assert!(ahead_from("not json").is_empty());
        assert!(ahead_from("").is_empty());
    }

    #[test]
    fn a_number_goes_straight_to_its_view() {
        use screen::Key;
        let mut pane = Pane {
            showing: Showing::Projects,
            only: None,
            projects: Vec::new(),
            repositories: vec!["one".to_string()],
            picked: 0,
        };
        assert_eq!(pane.press(Key::Char(b'3')), Asks::Draw);
        assert_eq!(pane.showing, Showing::Relations);
        assert_eq!(pane.press(Key::Char(b'4')), Asks::Draw);
        assert_eq!(pane.showing, Showing::Agents);
        assert_eq!(pane.press(Key::Char(b'2')), Asks::Draw);
        assert_eq!(pane.showing, Showing::Graph("one".to_string()));
        assert_eq!(pane.press(Key::Char(b'1')), Asks::Draw);
        assert_eq!(pane.showing, Showing::Projects);
        // With no repository to pick there is no graph to go to.
        pane.repositories.clear();
        assert_eq!(pane.press(Key::Char(b'2')), Asks::Nothing);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn in_ghostty_the_pane_goes_beside_the_terminal_it_was_typed_in() {
        let env = |name: &str| (name == "TERM_PROGRAM").then(|| "ghostty".to_string());
        let (program, args) = opener(
            Path::new("/bin/toimija"),
            Path::new("/work"),
            None,
            &env,
            Some("/dev/ttys004"),
            None,
        )
        .unwrap();
        assert_eq!(program, "osascript");
        assert!(
            args[1].contains("set here to first terminal whose tty is \"/dev/ttys004\""),
            "{}",
            args[1]
        );
        // Never the front window: with several open it may be another one.
        assert!(!args[1].contains("front window"), "{}", args[1]);
        // Closing asks the view to quit before the pane is taken away.
        let quit = args[1]
            .find("input text \"q\" to pane")
            .expect("the view is asked to quit");
        let close = args[1].find("close pane").expect("the pane is closed");
        assert!(quit < close, "{}", args[1]);

        // With no terminal of its own it refuses rather than guess a window.
        let refused = opener(
            Path::new("/bin/toimija"),
            Path::new("/work"),
            None,
            &env,
            None,
            None,
        )
        .unwrap_err();
        assert!(
            refused.contains("no telling which Ghostty window"),
            "{refused}"
        );
    }

    #[test]
    fn a_terminal_that_cannot_be_asked_says_what_to_run() {
        let said = opener(
            Path::new("/bin/toimija"),
            Path::new("/work"),
            None,
            &|_| None,
            None,
            None,
        )
        .unwrap_err();
        assert!(said.contains("/bin/toimija side"), "{said}");
    }
}
