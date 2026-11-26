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
//! edits, which touch no file git keeps.

use std::collections::BTreeMap;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result};

use crate::command::CommandRunner;
use crate::project::{self, Asked, View};
use crate::workspace;

/// The width asked for when this command opens its own pane, in columns.
pub const PANE_COLUMNS: u16 = 38;
const TICK: Duration = Duration::from_millis(1500);
/// How often uncommitted edits are looked for where a session is at work.
const BUSY_RECHECK: Duration = Duration::from_secs(20);
/// And where nobody is.
const QUIET_RECHECK: Duration = Duration::from_secs(180);

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
        out.write_all(b"\x1b[?1049h\x1b[?25l")?;
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

    // The projects to step through with `p`: all of them, then each.
    let names: Vec<String> = workspace::load(root)?
        .projects
        .into_iter()
        .map(|project| project.name)
        .collect();
    let mut only = options.project.clone();
    let mut memory = Memory::default();
    let _screen = screen::enter().context("the terminal could not be prepared")?;

    let mut drawn: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        // An idle tick: nothing moved and the view is recent. Wait for a
        // key and look again, without reading or drawing anything.
        let idle = drawn.is_some_and(|at| at.elapsed() < BUSY_RECHECK)
            && !resized.load(Ordering::Relaxed)
            && !memory.moved(root);
        if idle {
            match screen::key(TICK) {
                Some(b'q') => return Ok(0),
                Some(b'r') => {
                    memory.clear();
                    drawn = None;
                }
                Some(b'p' | b'\t') => {
                    only = next_project(&names, only.as_deref());
                    drawn = None;
                }
                _ => {}
            }
            continue;
        }
        drawn = Some(Instant::now());
        let (columns, rows) = screen::size().unwrap_or((usize::from(PANE_COLUMNS), 40));
        let width = options.width.unwrap_or(columns).min(columns);
        let body = match memory.view(
            runner,
            root,
            only.as_deref(),
            Instant::now(),
            &mut |names| project::ask_many(root, names),
        ) {
            Ok(view) => project::render_lines(&view, width),
            Err(error) => vec![format!("not read: {error:#}")],
        };
        draw(root, only.as_deref(), &body, width, rows)?;
        resized.store(false, Ordering::Relaxed);

        let until = Instant::now() + TICK;
        while Instant::now() < until
            && !stop.load(Ordering::Relaxed)
            && !resized.load(Ordering::Relaxed)
        {
            match screen::key(until.saturating_duration_since(Instant::now())) {
                Some(b'q') => return Ok(0),
                Some(b'r') => {
                    memory.clear();
                    break;
                }
                Some(b'p' | b'\t') => {
                    only = next_project(&names, only.as_deref());
                    break;
                }
                _ => {}
            }
        }
    }
    Ok(0)
}

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
fn draw(root: &Path, only: Option<&str>, body: &[String], width: usize, rows: usize) -> Result<()> {
    let folder = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = format!("TOIMIJA  {}", only.unwrap_or(&folder));
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let clock = format!(
        "{:02}:{:02}:{:02} UTC",
        now % 86_400 / 3_600,
        now % 3_600 / 60,
        now % 60
    );
    let rule: String = "─".repeat(width);
    let keys = "r again  p project  q quit";
    let room = rows.saturating_sub(4);

    let mut frame = String::from("\x1b[?2026h\x1b[H");
    let mut line = |text: &str| {
        let cut: String = text.chars().take(width).collect();
        frame.push_str(&cut);
        frame.push_str("\x1b[K\r\n");
    };
    line(&title);
    line(&rule);
    for text in body.iter().take(room) {
        line(text);
    }
    if body.len() > room {
        line(&format!(
            "… {} more lines; widen or pick a project",
            body.len() - room
        ));
    }
    frame.push_str("\x1b[J");
    // The last row: when this was read, and the keys.
    frame.push_str(&format!("\x1b[{rows};1H"));
    let foot: String = format!("{clock}  {keys}").chars().take(width).collect();
    frame.push_str(&foot);
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
        return Ok((
            "tmux".to_string(),
            vec![
                "split-window".to_string(),
                "-h".to_string(),
                "-d".to_string(),
                "-l".to_string(),
                PANE_COLUMNS.to_string(),
                "-c".to_string(),
                root.display().to_string(),
                shell_line,
            ],
        ));
    }
    if cfg!(target_os = "macos") && env("TERM_PROGRAM").as_deref() == Some("ghostty") {
        let script = format!(
            "tell application \"Ghostty\"\n\
             set here to focused terminal of selected tab of front window\n\
             set cfg to new surface configuration\n\
             set command of cfg to {}\n\
             set initial working directory of cfg to {}\n\
             set pane to split here direction right with configuration cfg\n\
             focus here\n\
             end tell",
            applescript_string(&shell_line),
            applescript_string(&root.display().to_string()),
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
    fn inside_tmux_the_pane_is_a_tmux_split_that_keeps_focus() {
        let env = |name: &str| (name == "TMUX").then(|| "/tmp/tmux-1/default,1,0".to_string());
        let (program, args) =
            opener(Path::new("/bin/toimija"), Path::new("/work"), None, &env).unwrap();
        assert_eq!(program, "tmux");
        assert_eq!(
            args,
            [
                "split-window",
                "-h",
                "-d",
                "-l",
                "38",
                "-c",
                "/work",
                "/bin/toimija side"
            ]
        );
    }

    #[test]
    fn a_project_name_cannot_become_shell_or_applescript() {
        let env = |name: &str| (name == "TMUX").then(|| "x".to_string());
        let hostile = "a'; rm -rf ~; \"";
        let (_, args) = opener(
            Path::new("/bin/toimija"),
            Path::new("/work"),
            Some(hostile),
            &env,
        )
        .unwrap();
        assert_eq!(
            args.last().unwrap(),
            "/bin/toimija side --project 'a'\\''; rm -rf ~; \"'"
        );
        assert_eq!(applescript_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn a_terminal_that_cannot_be_asked_says_what_to_run() {
        let said = opener(Path::new("/bin/toimija"), Path::new("/work"), None, &|_| {
            None
        })
        .unwrap_err();
        assert!(said.contains("/bin/toimija side"), "{said}");
    }
}
