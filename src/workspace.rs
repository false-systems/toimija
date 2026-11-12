//! A workspace: a folder that holds repositories and says so with
//! `.toimija/workspace.yaml`.
//!
//! Outside any Git repository, the nearest such file above the current
//! directory names the workspace. Its packet is built by reading files only:
//! the workspace file, each repository's `HEAD`, `.toimija/repo.yaml`,
//! session markers and handover. No git process is started for it, so the
//! cost does not grow with a git call per repository. What a file cannot
//! tell (uncommitted work, validation) is not in the packet; `toimija
//! enter` inside a repository has it.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::command::CommandRunner;
use crate::config::RepoConfig;
use crate::session::{self, Handover};
use crate::sessions::{self, Liveness, SessionView};

/// The file that makes a folder a workspace, relative to the folder.
pub const WORKSPACE_PATH: &str = ".toimija/workspace.yaml";

/// The schema of the JSON workspace packet.
pub const SCHEMA: &str = "toimija-workspace.v1";

/// One line saying what the packet leaves out and where it is.
pub const NOT_INCLUDED: &str = "Uncommitted work and validation are per repository: \
run `toimija enter` inside a repository for them.";

/// `.toimija/workspace.yaml`. Every field is optional, and an empty file is
/// a workspace. A field this version does not know is refused by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceFile {
    pub role: Option<String>,
    pub read_first: Vec<PathBuf>,
}

/// The nearest folder at or above `cwd` that holds a workspace file. The
/// walk starts from the current directory, canonicalized, so a symlinked
/// path and its target find the same workspace.
pub fn find(cwd: &Path) -> Option<PathBuf> {
    let start = cwd.canonicalize().ok()?;
    start
        .ancestors()
        .find(|folder| folder.join(WORKSPACE_PATH).is_file())
        .map(Path::to_path_buf)
}

/// Read and parse the workspace file of `root`.
pub fn load(root: &Path) -> Result<WorkspaceFile> {
    let path = root.join(WORKSPACE_PATH);
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read workspace file at {}", path.display()))?;
    // An empty file, or one of comments only, is YAML's null: a workspace
    // with nothing declared.
    let file: Option<WorkspaceFile> = serde_yaml::from_str(&text)
        .with_context(|| format!("invalid workspace file at {}", path.display()))?;
    Ok(file.unwrap_or_default())
}

/// The workspace packet. Paths are strings as a person reads them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspacePacket {
    pub schema: &'static str,
    pub workspace: WorkspaceBlock,
    /// The repositories directly inside the folder: those with an open
    /// session or a waiting handover first, then by name.
    pub repositories: Vec<Repository>,
    /// Sessions open in the workspace folder itself, outside any repository.
    pub sessions: Vec<OpenSession>,
    /// What could not be read, each with its reason. Never dropped silently.
    pub not_read: Vec<NotRead>,
    pub not_included: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceBlock {
    pub folder: String,
    pub role: Option<String>,
    pub read_first: Vec<ReadFirst>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadFirst {
    pub path: String,
    pub missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Repository {
    pub name: String,
    /// From the repository's own `.toimija/repo.yaml`. `None` when it has
    /// none, names no role, or cannot be read (then `not_read` says why).
    pub role: Option<String>,
    /// The branch `HEAD` names, or `detached at <short id>`. `None` when
    /// `HEAD` cannot be read: unknown, never a guess.
    pub branch: Option<String>,
    pub sessions: Vec<OpenSession>,
    pub handover: Option<WaitingHandover>,
    /// Whether `role` is unknown because its file could not be read, as
    /// opposed to not set. Rendered, not serialized twice.
    #[serde(skip)]
    role_unreadable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenSession {
    pub id: String,
    pub agent: String,
    /// `live` or `stale`.
    pub liveness: &'static str,
    pub intent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaitingHandover {
    pub written_at: u64,
    pub first_line: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotRead {
    pub what: String,
    pub reason: String,
}

/// Build the packet for the workspace at `root`. `runner` is used only to
/// ask whether a session's process is alive; nothing here starts git.
pub fn build(runner: &dyn CommandRunner, root: &Path) -> Result<WorkspacePacket> {
    let file = load(root)?;
    let mut not_read = Vec::new();

    let read_first = file
        .read_first
        .iter()
        .map(|path| ReadFirst {
            path: crate::path_codec::display(path),
            missing: !root.join(path).exists(),
        })
        .collect();

    let mut repositories = Vec::new();
    let mut entries: Vec<PathBuf> = match fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(|entry| match entry {
                Ok(entry) => Some(entry.path()),
                Err(error) => {
                    not_read.push(NotRead {
                        what: "a folder entry".to_string(),
                        reason: error.to_string(),
                    });
                    None
                }
            })
            .collect(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", root.display()));
        }
    };
    entries.sort();
    for dir in &entries {
        if fs::symlink_metadata(dir.join(".git")).is_err() {
            continue;
        }
        repositories.push(repository(runner, dir, &mut not_read));
    }
    repositories.sort_by(|a, b| {
        let waiting = |repository: &Repository| {
            repository.sessions.is_empty() && repository.handover.is_none()
        };
        waiting(a)
            .cmp(&waiting(b))
            .then_with(|| a.name.cmp(&b.name))
    });

    let sessions = folder_sessions(runner, root, &mut not_read);

    Ok(WorkspacePacket {
        schema: SCHEMA,
        workspace: WorkspaceBlock {
            folder: crate::path_codec::display(root),
            role: file.role.filter(|role| !role.trim().is_empty()),
            read_first,
        },
        repositories,
        sessions,
        not_read,
        not_included: NOT_INCLUDED,
    })
}

fn repository(runner: &dyn CommandRunner, dir: &Path, not_read: &mut Vec<NotRead>) -> Repository {
    let name = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut note = |what: &str, reason: String| {
        not_read.push(NotRead {
            what: format!("{name}: {what}"),
            reason,
        });
    };

    let (role, role_unreadable) = match RepoConfig::load_optional(dir) {
        Ok(config) => (
            config
                .map(|config| config.repo.role)
                .filter(|role| !role.trim().is_empty()),
            false,
        ),
        Err(error) => {
            note("role", error.to_string());
            (None, true)
        }
    };

    let branch = match branch(dir) {
        Ok(branch) => Some(branch),
        Err(reason) => {
            note("branch", reason);
            None
        }
    };

    let sessions = match sessions::report(runner, dir) {
        Ok(report) => {
            if let Some(reason) = report.coverage.reason.as_deref() {
                note("sessions", reason.to_string());
            }
            report.sessions.iter().map(open_session).collect()
        }
        Err(error) => {
            note("sessions", format!("{error:#}"));
            Vec::new()
        }
    };

    let handover = match read_handover(dir) {
        Ok(handover) => handover,
        Err(reason) => {
            note("handover", reason);
            None
        }
    };

    Repository {
        name,
        role,
        branch,
        sessions,
        handover,
        role_unreadable,
    }
}

/// The branch a repository's `HEAD` file names, read without git. For a
/// `.git` file (a linked worktree or a submodule), `gitdir:` leads to the
/// directory whose `HEAD` it is.
fn branch(dir: &Path) -> std::result::Result<String, String> {
    let dot_git = dir.join(".git");
    let metadata =
        fs::metadata(&dot_git).map_err(|error| format!(".git cannot be read: {error}"))?;
    let git_dir = if metadata.is_dir() {
        dot_git
    } else {
        let text = fs::read_to_string(&dot_git)
            .map_err(|error| format!(".git cannot be read: {error}"))?;
        let target = text
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:"))
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .ok_or_else(|| ".git is a file with no `gitdir:` line".to_string())?;
        dir.join(target)
    };
    let head = fs::read_to_string(git_dir.join("HEAD"))
        .map_err(|error| format!("HEAD cannot be read: {error}"))?;
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        if name.is_empty() {
            return Err("HEAD names no ref".to_string());
        }
        return Ok(name.to_string());
    }
    let is_object_id = matches!(head.len(), 40 | 64) && head.bytes().all(|b| b.is_ascii_hexdigit());
    if is_object_id {
        return Ok(format!("detached at {}", &head[..7]));
    }
    Err("HEAD is neither a ref nor a commit id".to_string())
}

/// A listed session: live or stale.
fn open_session(view: &SessionView) -> OpenSession {
    OpenSession {
        id: view.marker.id.clone(),
        agent: view.agent_label().to_string(),
        liveness: match view.liveness {
            Liveness::Active => "live",
            Liveness::Stale => "stale",
        },
        intent: view.marker.intent.clone(),
    }
}

/// The handover waiting in `dir`, with its first line. A missing file is no
/// handover; one that cannot be read is an error with its reason.
fn read_handover(dir: &Path) -> std::result::Result<Option<WaitingHandover>, String> {
    let path = dir.join(session::HANDOVER_FILE);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{error}")),
    };
    let handover: Handover =
        serde_json::from_str(&raw).map_err(|error| format!("malformed JSON: {error}"))?;
    let first_line = [&handover.unfinished, &handover.next, &handover.surprised]
        .into_iter()
        .flatten()
        .flat_map(|text| text.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string();
    Ok(Some(WaitingHandover {
        written_at: handover.written_at,
        first_line,
    }))
}

/// Sessions `run` opened outside any repository whose folder is this
/// workspace or below it, and not inside a repository.
fn folder_sessions(
    runner: &dyn CommandRunner,
    root: &Path,
    not_read: &mut Vec<NotRead>,
) -> Vec<OpenSession> {
    let Some(state) = session::outside_root() else {
        not_read.push(NotRead {
            what: "sessions in this folder".to_string(),
            reason: format!(
                "no state directory: set {} or HOME to an absolute path",
                session::STATE_DIR_ENV
            ),
        });
        return Vec::new();
    };
    let report = match sessions::report(runner, &state) {
        Ok(report) => report,
        Err(error) => {
            not_read.push(NotRead {
                what: "sessions in this folder".to_string(),
                reason: format!("{error:#}"),
            });
            return Vec::new();
        }
    };
    if let Some(reason) = report.coverage.reason.as_deref() {
        not_read.push(NotRead {
            what: "sessions in this folder".to_string(),
            reason: reason.to_string(),
        });
    }
    report
        .sessions
        .iter()
        .filter(|view| in_workspace_outside_repositories(Path::new(&view.marker.worktree), root))
        .map(open_session)
        .collect()
}

/// Whether `folder` is `root` or below it, with no repository between the
/// two. Both sides are canonicalized before the comparison.
fn in_workspace_outside_repositories(folder: &Path, root: &Path) -> bool {
    let (Ok(folder), Ok(root)) = (folder.canonicalize(), root.canonicalize()) else {
        return false;
    };
    if !folder.starts_with(&root) {
        return false;
    }
    folder
        .ancestors()
        .take_while(|ancestor| *ancestor != root)
        .all(|ancestor| fs::symlink_metadata(ancestor.join(".git")).is_err())
}

/// The packet as Markdown.
pub fn render_markdown(packet: &WorkspacePacket) -> String {
    let mut out = String::new();
    let or = |value: &Option<String>, missing: &str| value.clone().unwrap_or(missing.to_string());
    writeln!(out, "# Toimija Workspace").unwrap();
    writeln!(out, "\n## Workspace").unwrap();
    writeln!(out, "- folder: {}", packet.workspace.folder).unwrap();
    writeln!(out, "- role: {}", or(&packet.workspace.role, "not set")).unwrap();

    writeln!(out, "\n## Read first").unwrap();
    if packet.workspace.read_first.is_empty() {
        writeln!(out, "- none").unwrap();
    }
    for entry in &packet.workspace.read_first {
        let missing = if entry.missing { " (missing)" } else { "" };
        writeln!(out, "- {}{missing}", entry.path).unwrap();
    }

    writeln!(out, "\n## Repositories").unwrap();
    if packet.repositories.is_empty() {
        writeln!(out, "- none").unwrap();
    }
    for repository in &packet.repositories {
        writeln!(out, "- {}", repository.name).unwrap();
        let no_role = if repository.role_unreadable {
            "unknown"
        } else {
            "not set"
        };
        writeln!(out, "  - role: {}", brief(&or(&repository.role, no_role))).unwrap();
        writeln!(out, "  - branch: {}", or(&repository.branch, "unknown")).unwrap();
        if repository.sessions.is_empty() {
            writeln!(out, "  - sessions: none").unwrap();
        }
        for session in &repository.sessions {
            writeln!(out, "  - session: {}", session_line(session)).unwrap();
        }
        match &repository.handover {
            Some(handover) => {
                writeln!(out, "  - handover: {}", brief(&handover.first_line)).unwrap()
            }
            None => writeln!(out, "  - handover: none").unwrap(),
        }
    }

    writeln!(out, "\n## Sessions in this folder").unwrap();
    if packet.sessions.is_empty() {
        writeln!(out, "- none").unwrap();
    }
    for session in &packet.sessions {
        writeln!(out, "- {}", session_line(session)).unwrap();
    }

    if !packet.not_read.is_empty() {
        writeln!(out, "\n## Not read").unwrap();
        for item in &packet.not_read {
            writeln!(out, "- {}: {}", item.what, item.reason).unwrap();
        }
    }

    writeln!(out, "\n## Not in this packet").unwrap();
    writeln!(out, "{}", packet.not_included).unwrap();
    out
}

/// The longest role, intent or handover line shown for one repository. The
/// JSON packet carries each whole.
const BRIEF_WIDTH: usize = 100;

/// One line of a repository's entry, short enough that a folder of many
/// repositories stays a packet an agent reads whole.
fn brief(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    match line.char_indices().nth(BRIEF_WIDTH) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.to_string(),
    }
}

fn session_line(session: &OpenSession) -> String {
    format!(
        "{} ({}): {}",
        session.agent,
        session.liveness,
        brief(session.intent.as_deref().unwrap_or("unknown"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn a_long_role_intent_or_handover_is_cut_to_one_short_line() {
        let long = "x".repeat(BRIEF_WIDTH + 40);
        let cut = brief(&long);
        assert_eq!(cut.chars().count(), BRIEF_WIDTH + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(brief("first line\nsecond line"), "first line");
        assert_eq!(brief("short"), "short");
        assert_eq!(
            brief(&"é".repeat(BRIEF_WIDTH + 5)).chars().count(),
            BRIEF_WIDTH + 1
        );
    }

    #[test]
    fn head_is_read_as_a_branch_a_detached_id_or_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(root, "a/.git/HEAD", "ref: refs/heads/feat/login\n");
        assert_eq!(branch(&root.join("a")).unwrap(), "feat/login");

        let sha256 = "0123456789abcdef".repeat(4);
        write(root, "b/.git/HEAD", &format!("{sha256}\n"));
        assert_eq!(branch(&root.join("b")).unwrap(), "detached at 0123456");

        write(root, "c/.git/HEAD", "something else\n");
        assert!(branch(&root.join("c")).is_err());

        write(root, "d/.git", "not a pointer\n");
        assert!(branch(&root.join("d")).unwrap_err().contains("gitdir"));

        // An absolute `gitdir:` is followed as it stands.
        write(root, "store/HEAD", "ref: refs/heads/main\n");
        write(
            root,
            "e/.git",
            &format!("gitdir: {}\n", root.join("store").display()),
        );
        assert_eq!(branch(&root.join("e")).unwrap(), "main");
    }

    #[test]
    fn a_file_of_comments_or_nothing_is_an_empty_workspace() {
        let temp = tempfile::tempdir().unwrap();
        write(
            temp.path(),
            WORKSPACE_PATH,
            "# What this folder is for:\n# role: the services of one product\n",
        );
        assert_eq!(load(temp.path()).unwrap(), WorkspaceFile::default());
        write(temp.path(), WORKSPACE_PATH, "");
        assert_eq!(load(temp.path()).unwrap(), WorkspaceFile::default());
        write(temp.path(), WORKSPACE_PATH, "rolee: x\n");
        let error = format!("{:#}", load(temp.path()).unwrap_err());
        assert!(error.contains("rolee"), "{error}");
    }

    #[test]
    fn a_session_folder_counts_only_outside_repositories_and_inside_the_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("work");
        fs::create_dir_all(root.join("plain/deeper")).unwrap();
        write(&root, "repo/.git/HEAD", "ref: refs/heads/main\n");
        fs::create_dir_all(root.join("repo/src")).unwrap();
        fs::create_dir_all(temp.path().join("elsewhere")).unwrap();

        assert!(in_workspace_outside_repositories(&root, &root));
        assert!(in_workspace_outside_repositories(
            &root.join("plain/deeper"),
            &root
        ));
        assert!(!in_workspace_outside_repositories(
            &root.join("repo/src"),
            &root
        ));
        assert!(!in_workspace_outside_repositories(
            &temp.path().join("elsewhere"),
            &root
        ));
        assert!(!in_workspace_outside_repositories(
            &root.join("gone"),
            &root
        ));
    }
}
