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

mod tools;

/// The width asked for when this command opens its own pane, in columns.
pub const PANE_COLUMNS: u16 = 38;
/// The title the view gives its pane, by which `--open` finds it again.
pub const PANE_TITLE: &str = "toimija side";
const TICK: Duration = Duration::from_millis(1500);
/// How often uncommitted edits are looked for where a session is at work.
const BUSY_RECHECK: Duration = Duration::from_secs(20);
/// And where nobody is.
const QUIET_RECHECK: Duration = Duration::from_secs(180);
/// How often vessa and ohjaa are asked again.
const TOOLS_RECHECK: Duration = Duration::from_secs(120);
/// Marks a line of the pane as a section heading, for [`draw`] to set bold.
const HEAD: char = '\u{e000}';

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

    /// One key, waiting at most `wait`. `None` when none came.
    pub fn key(wait: std::time::Duration) -> Option<u8> {
        let mut watched = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = i32::try_from(wait.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: one valid pollfd, and a one-byte buffer for read.
        unsafe {
            if libc::poll(&mut watched, 1, millis) <= 0 {
                return None;
            }
            let mut byte = 0u8;
            (libc::read(libc::STDIN_FILENO, (&raw mut byte).cast(), 1) == 1).then_some(byte)
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
    /// The views by number, the one on screen in brackets.
    fn tabs(&self) -> String {
        let mark = |on: bool, text: &str| {
            if on {
                format!("[{text}]")
            } else {
                text.to_string()
            }
        };
        [
            mark(matches!(self, Showing::Projects), "1 projects"),
            mark(matches!(self, Showing::Graph(_)), "2 graph"),
            mark(matches!(self, Showing::Relations), "3 links"),
            mark(matches!(self, Showing::Agents), "4 agents"),
        ]
        .join(" ")
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

/// The branch graph of the repository at `dir`, as lines for the pane.
fn graph_lines(runner: &dyn CommandRunner, dir: &Path, width: usize) -> Vec<String> {
    match crate::graph::build(runner, dir, "origin/main", &crate::graph::sykli_proof) {
        Ok(graph) => crate::graph::render_narrow(&graph, width),
        Err(error) => vec![format!("not read: {error:#}")],
    }
}

/// The graphs already asked for, by repository: the files each rested on,
/// the width it was drawn for, and its lines.
type Graphs = std::collections::HashMap<String, (Fingerprint, usize, Vec<String>)>;

/// A section heading: its label, then a rule to the pane's edge.
fn section(label: &str, width: usize) -> String {
    let rule = "─".repeat(width.saturating_sub(label.chars().count() + 1));
    format!("{HEAD}{label} {rule}")
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

/// The agent sessions, at most `limit`, or why there are none to show.
fn agents_body(facts: Option<&tools::Facts>, width: usize, limit: usize) -> Vec<String> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    match facts.map(|facts| &facts.agents) {
        None => vec!["reading…".to_string()],
        Some(Ok(agents)) => tools::agents_lines(agents, now, width, limit),
        Some(Err(why)) => project::wrap(&format!("not read: {why}"), "", width),
    }
}

/// ohjaa's summary, or why there is none.
fn knowledge_body(facts: Option<&tools::Facts>, width: usize) -> Vec<String> {
    match facts.map(|facts| &facts.knowledge) {
        None => vec!["reading…".to_string()],
        Some(Ok(summary)) => project::wrap(summary, "", width),
        Some(Err(why)) => project::wrap(&format!("not read: {why}"), "", width),
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
    // The graph last drawn: whose it is, the files it rests on, its lines.
    let mut graphs: Graphs = Graphs::new();
    let mut tools = Tools::default();
    let _screen = screen::enter().context("the terminal could not be prepared")?;
    if options.fit {
        fit_pane();
    }

    let mut drawn: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        // An idle tick: nothing moved and the view is recent. Wait for a
        // key and look again, without reading or drawing anything.
        if tools.due() {
            tools.ask(root);
        }
        let idle = drawn.is_some_and(|at| at.elapsed() < BUSY_RECHECK)
            && !tools.arrived.load(Ordering::SeqCst)
            && !resized.load(Ordering::Relaxed)
            && !memory.moved(root);
        if idle {
            match screen::read_key(TICK).map(|key| pane.press(key)) {
                Some(Asks::Quit) => return Ok(0),
                Some(Asks::Again) => {
                    memory.clear();
                    graphs.clear();
                    tools.asked = None;
                    drawn = None;
                }
                Some(Asks::Draw) => drawn = None,
                Some(Asks::Nothing) | None => {}
            }
            continue;
        }
        drawn = Some(Instant::now());
        resized.store(false, Ordering::Relaxed);
        tools.arrived.store(false, Ordering::SeqCst);
        let facts = tools.facts();
        let (columns, rows) = screen::size().unwrap_or((usize::from(PANE_COLUMNS), 40));
        let width = options.width.unwrap_or(columns).min(columns);

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
        let folder = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = match &pane.showing {
            Showing::Graph(name) => name.clone(),
            Showing::Projects | Showing::Relations | Showing::Agents => {
                pane.only.clone().unwrap_or(folder)
            }
        };
        let (mut body, keys) = match &pane.showing {
            Showing::Projects => (
                match &view {
                    Ok(view) => project::render_rows(view, width, pane.picked()),
                    Err(error) => vec![format!("not read: {error:#}")],
                },
                "jk pick  p next project  r  q quit",
            ),
            Showing::Relations => (
                match &view {
                    Ok(view) => {
                        let lines = project::render_relations(view, width);
                        if lines.is_empty() {
                            vec!["none declared between these".to_string()]
                        } else {
                            lines
                        }
                    }
                    Err(error) => vec![format!("not read: {error:#}")],
                },
                "p next project  r again  q quit",
            ),
            Showing::Graph(_) => (Vec::new(), "1 back  r again  q quit"),
            Showing::Agents => {
                let mut lines = vec![section("LAST DAY, BY AGENT", width)];
                match facts.as_ref().map(|facts| &facts.agents) {
                    Some(Ok(agents)) => lines.extend(tools::totals_lines(agents)),
                    _ => lines.push("…".to_string()),
                }
                lines.push(String::new());
                lines.push(section("SESSIONS", width));
                lines.extend(agents_body(facts.as_ref(), width, usize::MAX));
                lines.push(String::new());
                lines.push(section("KNOWLEDGE", width));
                lines.extend(knowledge_body(facts.as_ref(), width));
                (lines, "1 back  r again  q quit")
            }
        };
        if pane.showing == Showing::Projects {
            body.push(String::new());
            body.push(section("AGENTS", width));
            body.extend(agents_body(facts.as_ref(), width, 4));
            body.push(String::new());
            body.push(section("KNOWLEDGE", width));
            body.extend(knowledge_body(facts.as_ref(), width));
        }
        // The graph is on screen without a key: under the projects it is the
        // picked repository's, and view 2 gives it the whole pane.
        let of = match &pane.showing {
            Showing::Projects => pane.picked().map(str::to_string),
            Showing::Graph(name) => Some(name.clone()),
            Showing::Relations | Showing::Agents => None,
        };
        if let Some(name) = of {
            if pane.showing == Showing::Projects {
                body.push(String::new());
                body.push(section(&format!("GRAPH {name}"), width));
            }
            let dir = root.join(&name);
            let now = fingerprint(&dir);
            let fresh = graphs
                .get(&name)
                .is_some_and(|(at, wide, _)| *at == now && *wide == width);
            if !fresh {
                // Asking takes a moment in a clone with many branches.
                let mut waiting = body.clone();
                waiting.push("reading…".to_string());
                draw(&title, &pane.showing.tabs(), &waiting, keys, width, rows)?;
                let lines = graph_lines(runner, &dir, width);
                graphs.insert(name.clone(), (now, width, lines));
            }
            if let Some((_, _, lines)) = graphs.get(&name) {
                body.extend(lines.iter().cloned());
            }
        }
        draw(&title, &pane.showing.tabs(), &body, keys, width, rows)?;
    }
    Ok(0)
}

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

/// One line of the pane, cut to its width and then coloured: a heading
/// bold, the picked repository in reverse, the words that say how a commit
/// stands in green, yellow or red, and the detail under a repository dim.
/// The colours are the terminal's own, so they follow its theme.
fn styled(text: &str, width: usize) -> String {
    if let Some(heading) = text.strip_prefix(HEAD) {
        let cut: String = heading.chars().take(width).collect();
        return format!("\x1b[1m{cut}\x1b[0m");
    }
    let cut: String = text.chars().take(width).collect();
    if cut.ends_with(project::PICKED) {
        return format!("\x1b[7m{cut}\x1b[0m");
    }
    let mut out = cut.clone();
    for (word, colour) in [
        (" proven", "32"),
        (" now ", "32"),
        ("●", "32"),
        (" failed", "31"),
        ("blocking", "31"),
        (" unknown", "33"),
        (" not run", "33"),
        (" no gate", "33"),
        ("not read", "33"),
        ("partial:", "33"),
    ] {
        out = out.replace(word, &format!("\x1b[{colour}m{word}\x1b[39m"));
    }
    if cut.starts_with("    ") || cut.starts_with("… ") {
        format!("\x1b[2m{out}\x1b[0m")
    } else {
        out
    }
}

/// One frame, written once, inside the terminal's synchronized-output marks
/// so a half-drawn frame is never shown. The picked repository's line is
/// drawn in reverse as well as marked.
#[cfg(unix)]
fn draw(
    title: &str,
    tabs: &str,
    body: &[String],
    keys: &str,
    width: usize,
    rows: usize,
) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let clock = format!("{:02}:{:02} UTC", now % 86_400 / 3_600, now % 3_600 / 60);
    let name = format!("TOIMIJA  {title}");
    let gap = width.saturating_sub(name.chars().count() + clock.chars().count());
    let heading = if gap >= 2 {
        format!("{name}{}{clock}", " ".repeat(gap))
    } else {
        name
    };
    let room = rows.saturating_sub(5);

    let mut frame = String::from("\x1b[?2026h\x1b[H");
    frame.push_str(&styled(&format!("{HEAD}{heading}"), width));
    frame.push_str("\x1b[K\r\n");
    let tabs: String = tabs.chars().take(width).collect();
    frame.push_str("\x1b[2m");
    frame.push_str(
        &tabs
            .replace('[', "\x1b[22;1;7m ")
            .replace(']', " \x1b[0;2m"),
    );
    frame.push_str("\x1b[0m\x1b[K\r\n");
    let mut line = |text: &str| {
        frame.push_str(&styled(text, width));
        frame.push_str("\x1b[K\r\n");
    };
    line("");
    for text in body.iter().take(room) {
        line(text);
    }
    if body.len() > room {
        line(&format!("… {} more lines", body.len() - room));
    }
    frame.push_str("\x1b[J");
    // The last row: the keys.
    frame.push_str(&format!("\x1b[{rows};1H"));
    let foot: String = keys.chars().take(width).collect();
    frame.push_str("\x1b[2m");
    frame.push_str(&foot);
    frame.push_str("\x1b[0m\x1b[K\x1b[?2026l");

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
) -> std::result::Result<(String, Vec<String>), String> {
    let mut side = vec![exe.display().to_string(), "side".to_string()];
    if let Some(project) = project {
        side.push("--project".to_string());
        side.push(project.to_string());
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
                 {shell_line} --open"
            ));
        };
        // In the tab this terminal is in, a pane titled as the view titles
        // itself is the view: close it. Otherwise open one beside this
        // terminal and give the focus back.
        let script = format!(
            "tell application \"Ghostty\"\n\
             set here to first terminal whose tty is {tty}\n\
             set panes to {{}}\n\
             repeat with w in windows\n\
             repeat with t in tabs of w\n\
             if (tty of terminals of t) contains {tty} then\n\
             set panes to (terminals of t whose name is {title})\n\
             end if\n\
             end repeat\n\
             end repeat\n\
             if (count of panes) > 0 then\n\
             repeat with pane in panes\n\
             close pane\n\
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
         {shell_line}\n\
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
    fn inside_tmux_open_is_a_toggle_that_keeps_focus() {
        let env = |name: &str| (name == "TMUX").then(|| "/tmp/tmux-1/default,1,0".to_string());
        let (program, args) = opener(
            Path::new("/bin/toimija"),
            Path::new("/work"),
            None,
            &env,
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

    #[test]
    fn the_views_are_numbered_and_the_one_on_screen_is_bracketed() {
        assert_eq!(
            Showing::Projects.tabs(),
            "[1 projects] 2 graph 3 links 4 agents"
        );
        assert_eq!(
            Showing::Graph("one".to_string()).tabs(),
            "1 projects [2 graph] 3 links 4 agents"
        );
        assert!(Showing::Relations.tabs().contains("[3 links]"));
        assert!(Showing::Agents.tabs().ends_with("[4 agents]"));
        // Whichever is on screen, the line fits the pane.
        assert!(Showing::Agents.tabs().chars().count() <= usize::from(PANE_COLUMNS));
    }

    #[test]
    fn a_heading_is_bold_to_the_panes_edge_and_words_get_their_colour() {
        let heading = section("GRAPH sykli", 20);
        assert_eq!(heading.chars().count(), 21, "the mark and twenty columns");
        assert_eq!(styled(&heading, 20), "\x1b[1mGRAPH sykli ────────\x1b[0m");
        assert_eq!(
            styled("|    proven  review: none", 40),
            "|   \x1b[32m proven\x1b[39m  review: none"
        );
        assert!(styled("    main  clean  failed", 40).starts_with("\x1b[2m"));
        assert!(styled("    main  clean  failed", 40).contains("\x1b[31m failed"));
        // Cut before it is coloured, so a colour never counts as width.
        assert_eq!(styled("abc proven", 6), "abc pr");
        let picked = format!("○ one {}", project::PICKED);
        assert_eq!(styled(&picked, 40), format!("\x1b[7m{picked}\x1b[0m"));
    }

    #[test]
    fn what_a_tool_did_not_say_is_said_not_left_blank() {
        assert_eq!(agents_body(None, 38, 4), ["reading…"]);
        let facts = tools::Facts {
            agents: Err("vessa is not installed".to_string()),
            knowledge: Ok("3 current, 1 waiting for a decision".to_string()),
        };
        assert_eq!(
            agents_body(Some(&facts), 38, 4),
            ["not read: vessa is not installed"]
        );
        assert_eq!(
            knowledge_body(Some(&facts), 38),
            ["3 current, 1 waiting for a decision"]
        );
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

        // With no terminal of its own it refuses rather than guess a window.
        let refused = opener(
            Path::new("/bin/toimija"),
            Path::new("/work"),
            None,
            &env,
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
        )
        .unwrap_err();
        assert!(said.contains("/bin/toimija side"), "{said}");
    }
}
