//! Session markers for `toimija run`, and the handover between agents.
//!
//! Each `run` session owns `.toimija/sessions/<id>/session.json`: who the
//! agent is, where it works, and why it says it is there. The marker lives
//! exactly as long as its `run`: written when the session opens, stamped once
//! a minute while the agent runs, and removed when the agent exits. No daemon,
//! no network, no git process: plain files, written by the command that owns
//! them.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Directory, relative to the repo root, holding all per-session state.
pub const SESSIONS_SUBDIR: &str = ".toimija/sessions";

/// Handshake variables that only mean something inside a repository. A
/// session outside one clears them for the child, so a value inherited from
/// an outer session cannot point it at another session's repository.
pub const REPO_ONLY_ENV: &[&str] = &["TOIMIJA_REPO_ROOT"];

/// Overrides where sessions opened outside any Git repository live.
pub const STATE_DIR_ENV: &str = "TOIMIJA_STATE_DIR";

/// Where sessions opened outside any Git repository live:
/// machine-local state, never a `.toimija/` folder in the directory the agent
/// works in. `$TOIMIJA_STATE_DIR`, else `$XDG_STATE_HOME/toimija`, else
/// `$HOME/.local/state/toimija`; only absolute paths count. `None` when none
/// resolves. Sessions sit under `<root>/.toimija/sessions/` like a repo's, so
/// every write still goes through the one `.toimija/` guard.
pub fn outside_root() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    absolute(STATE_DIR_ENV)
        .or_else(|| absolute("XDG_STATE_HOME").map(|path| path.join("toimija")))
        .or_else(|| absolute("HOME").map(|path| path.join(".local/state/toimija")))
}

/// Whether `root` holds a marker for session `id`. The id is checked for the
/// `<unix-secs>-<pid>` shape first, so it can never name a path.
pub fn marker_exists(root: &Path, id: &str) -> bool {
    is_valid_session_id(id)
        && root
            .join(SESSIONS_SUBDIR)
            .join(id)
            .join("session.json")
            .is_file()
}

/// On-disk session marker (`session.json`). Self-describing, so `toimija
/// sessions` needs nothing else to list it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMarker {
    pub id: String,
    pub pid: u32,
    /// When `pid` started, as the operating system records it: an opaque
    /// token, compared only for equality with what the same machine reads for
    /// that pid later (`crate::sessions::process_start`). A pid outlives its
    /// process; a live pid that started at another time is another process.
    /// `None` where the start cannot be read: the pid alone decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_start: Option<u64>,
    pub agent: Vec<String>,
    /// Absolute path of the folder the agent works in: a worktree, or a
    /// folder outside any repository. Distinguishes sessions that share a
    /// repository but check out separate worktrees.
    #[serde(default)]
    pub worktree: String,
    pub branch: Option<String>,
    /// HEAD when the session started. It stays fixed so a later packet can
    /// report drift.
    #[serde(default)]
    pub head: Option<String>,
    /// Free-text declaration of *why* the session is here. `None` renders as
    /// `unknown`. Advisory, not an assignment.
    #[serde(default)]
    pub intent: Option<String>,
    /// Zero or more declared paths the session expects to touch. Advisory
    /// only: never a lock, never ownership. Empty renders as `unknown`.
    #[serde(default)]
    pub scope: Vec<String>,
    pub started_at: u64,
    /// Unix seconds when `run` last stamped the marker: once a minute while
    /// its agent runs, and whenever the session declares its intent. `None`
    /// falls back to `started_at`.
    #[serde(default)]
    pub last_seen_at: Option<u64>,
    /// What `run` had for the agent but could not put in its context, because
    /// it has no delivery for that agent: `alert`, `packet`, `task`,
    /// `briefing`. Empty when everything was delivered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undelivered: Vec<String>,
}

/// Compose a session id as `<unix-secs>-<pid>`. Roughly sortable by
/// start time; the pid disambiguates sessions opened in the same second. Pure, so
/// tests are deterministic.
pub fn session_id(started_at: u64, pid: u32) -> String {
    format!("{started_at}-{pid}")
}

/// An open `run` session: its directory and marker.
pub struct Session {
    pub id: String,
    root: PathBuf,
    pub dir: PathBuf,
    marker_path: PathBuf,
    marker: SessionMarker,
}

impl Session {
    /// Open a new session under `repo_root` for the wrapped `agent` argv,
    /// carrying the optional `intent` and declared `scope`: create the
    /// session directory and write the marker.
    pub fn open(
        repo_root: &Path,
        agent: &[String],
        intent: Option<String>,
        scope: Vec<String>,
    ) -> Result<Session> {
        Self::open_at(repo_root, repo_root, agent, intent, scope)
    }

    /// Open a session for an agent working in `folder`, which is not inside
    /// any Git repository. The session lives under `state_root`
    /// (see [`outside_root`]) and records `folder` as its worktree.
    pub fn open_outside(
        state_root: &Path,
        folder: &Path,
        agent: &[String],
        intent: Option<String>,
        scope: Vec<String>,
    ) -> Result<Session> {
        fs::create_dir_all(state_root).with_context(|| {
            format!(
                "failed to create the state directory {}",
                state_root.display()
            )
        })?;
        Self::open_at(state_root, folder, agent, intent, scope)
    }

    /// Open a session under `root` whose agent works in `worktree`.
    fn open_at(
        root: &Path,
        worktree: &Path,
        agent: &[String],
        intent: Option<String>,
        scope: Vec<String>,
    ) -> Result<Session> {
        let started_at = now_secs();
        let pid = std::process::id();
        let id = session_id(started_at, pid);
        let dir = root.join(SESSIONS_SUBDIR).join(&id);
        crate::owned::ensure_dir(root, &dir).context("failed to open the session")?;
        let marker = SessionMarker {
            id: id.clone(),
            pid,
            pid_start: crate::sessions::process_start(pid),
            agent: agent.to_vec(),
            worktree: worktree.display().to_string(),
            branch: None,
            head: None,
            intent,
            scope,
            started_at,
            last_seen_at: Some(started_at),
            undelivered: Vec::new(),
        };
        let session = Session {
            id,
            root: root.to_path_buf(),
            marker_path: dir.join("session.json"),
            dir,
            marker,
        };
        session.write_marker()?;
        Ok(session)
    }

    /// The marker as this session wrote it last.
    pub fn marker(&self) -> &SessionMarker {
        &self.marker
    }

    /// Record the entry branch and HEAD, and rewrite the marker.
    pub fn record_git(&mut self, branch: Option<String>, head: Option<String>) -> Result<()> {
        self.marker.branch = branch;
        self.marker.head = head;
        self.write_marker()
    }

    /// Record what the agent was not handed (see [`SessionMarker::undelivered`]).
    pub fn record_undelivered(&mut self, items: Vec<String>) -> Result<()> {
        self.marker.undelivered = items;
        self.write_marker()
    }

    /// End the session: remove its directory and the marker in it, so no
    /// reader takes an ended session for a working one. Returns the marker as
    /// it stood on disk last, with what the agent declared while it ran
    /// (`toimija intent`), or as this session wrote it when that cannot be
    /// read back.
    pub fn end(self) -> Result<SessionMarker> {
        let last = load_marker(&self.dir)
            .ok()
            .filter(|on_disk| on_disk.id == self.id)
            .unwrap_or(self.marker);
        crate::owned::remove_dir(&self.root, &self.dir).context("failed to end the session")?;
        Ok(last)
    }

    /// The handshake the wrapped child inherits: who the session is, and the
    /// repository it runs in.
    pub fn env(&self) -> Vec<(&'static str, OsString)> {
        vec![
            ("TOIMIJA_RUN", OsString::from("1")),
            ("TOIMIJA_SESSION", OsString::from(&self.id)),
            ("TOIMIJA_RUN_ID", OsString::from(&self.id)),
            ("TOIMIJA_REPO_ROOT", self.root.as_os_str().to_os_string()),
        ]
    }

    /// The handshake for a session outside any repository: who it is, and
    /// where its marker lives. Pair with clearing [`REPO_ONLY_ENV`].
    pub fn env_outside(&self) -> Vec<(&'static str, OsString)> {
        vec![
            (STATE_DIR_ENV, self.root.clone().into_os_string()),
            ("TOIMIJA_RUN", OsString::from("1")),
            ("TOIMIJA_SESSION", OsString::from(&self.id)),
            ("TOIMIJA_RUN_ID", OsString::from(&self.id)),
        ]
    }

    fn write_marker(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.marker)?;
        crate::owned::replace(
            &self.root,
            &self.marker_path,
            format!("{json}\n").as_bytes(),
        )
    }
}

/// The `run` session this process was launched under, if any: a valid
/// `TOIMIJA_SESSION` id whose marker exists under *this* repo root and whose
/// process is not provably gone. Resolution is per-repo — an id inherited
/// from a session in another repo finds no marker here and resolves to
/// `None`, so a handshake can never steer writes into the wrong repo. Returns
/// the id and the session directory.
///
/// Unreadable ≠ missing: a marker that exists but cannot be read or parsed —
/// or whose recorded id disagrees with its directory — still resolves to
/// `None`, but it is warned about. Only a missing marker stays quiet.
pub fn ambient_session(repo_root: &Path) -> Option<(String, PathBuf)> {
    // Empty reads as unset: `run` clears handshake values that way for Codex.
    let id = std::env::var("TOIMIJA_SESSION").ok()?;
    if !is_valid_session_id(&id) {
        return None;
    }
    let dir = repo_root.join(SESSIONS_SUBDIR).join(&id);
    let marker_path = dir.join("session.json");
    let unreadable = |error: &dyn std::fmt::Display| {
        warn!(
            event = "toimija.session.marker_unreadable",
            path = %marker_path.display(),
            error = %error
        );
    };
    let raw = match fs::read_to_string(&marker_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            unreadable(&error);
            return None;
        }
    };
    let marker: SessionMarker = match serde_json::from_str(&raw) {
        Ok(marker) => marker,
        Err(error) => {
            unreadable(&error);
            return None;
        }
    };
    if marker.id != id {
        warn!(
            event = "toimija.session.marker_mismatch",
            path = %marker_path.display(),
            expected = %id,
            found = %marker.id
        );
        return None;
    }
    // A `run` killed before it could remove its marker (SIGKILL, OOM) leaves
    // the agent running with the handshake. That session is over.
    if crate::sessions::process_gone(repo_root, &marker) {
        warn!(
            event = "toimija.session.process_gone",
            session = %id,
            pid = marker.pid
        );
        return None;
    }
    Some((id, dir))
}

/// Read the marker in a session directory.
pub fn load_marker(session_dir: &Path) -> Result<SessionMarker> {
    let marker_path = session_dir.join("session.json");
    let raw = fs::read_to_string(&marker_path)
        .with_context(|| format!("failed to read {}", marker_path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", marker_path.display()))
}

/// The heartbeat: stamp `last_seen_at = now` on the marker of session `id`.
/// `run` calls it once a minute while its agent runs; it is a file write and
/// starts no process. A missing marker is left alone. An unreadable or
/// corrupt marker is an error — silence here means heartbeats stop stamping
/// and a live session gets misreported stale.
pub fn touch(root: &Path, id: &str) -> Result<()> {
    let marker_path = root.join(SESSIONS_SUBDIR).join(id).join("session.json");
    let raw = match fs::read_to_string(&marker_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", marker_path.display()));
        }
    };
    let mut marker: SessionMarker = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", marker_path.display()))?;
    marker.last_seen_at = Some(now_secs());
    let json = serde_json::to_string_pretty(&marker)?;
    crate::owned::replace(root, &marker_path, format!("{json}\n").as_bytes())
}

/// Whether `id` has the `<unix-secs>-<pid>` shape `run` mints.
/// `toimija intent` reads the id from the environment, so this is also the
/// guard that keeps a hostile `TOIMIJA_SESSION` from steering the marker path
/// out of `.toimija/sessions/`.
pub fn is_valid_session_id(id: &str) -> bool {
    match id.split_once('-') {
        Some((secs, pid)) => {
            !secs.is_empty()
                && !pid.is_empty()
                && secs.bytes().all(|byte| byte.is_ascii_digit())
                && pid.bytes().all(|byte| byte.is_ascii_digit())
        }
        None => false,
    }
}

/// Update the intent (and append declared scope paths) on an *existing*
/// session marker — the session belongs to `run`; this is the wrapped agent
/// reaching back into its own record via the `TOIMIJA_SESSION` handshake.
/// Advisory data only: never an assignment, never a lock. Returns the updated
/// marker.
pub fn declare_intent(
    root: &Path,
    id: &str,
    intent: String,
    scope: Vec<String>,
) -> Result<SessionMarker> {
    anyhow::ensure!(
        is_valid_session_id(id),
        "invalid session id `{id}` — expected the `<unix-secs>-<pid>` shape from TOIMIJA_SESSION"
    );
    let marker_path = root.join(SESSIONS_SUBDIR).join(id).join("session.json");
    let raw = fs::read_to_string(&marker_path).with_context(|| {
        format!(
            "no session marker at {} — is the session still open?",
            marker_path.display()
        )
    })?;
    let mut marker: SessionMarker = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", marker_path.display()))?;
    marker.intent = Some(intent);
    for path in scope {
        if !marker.scope.contains(&path) {
            marker.scope.push(path);
        }
    }
    // Declaring intent is evidence of life: stamp the heartbeat too.
    marker.last_seen_at = Some(now_secs());
    let json = serde_json::to_string_pretty(&marker)?;
    crate::owned::replace(root, &marker_path, format!("{json}\n").as_bytes())
        .context("failed to write the session marker")?;
    Ok(marker)
}

/// Where a working copy's handover lives: beside the session markers under
/// `.toimija/`, which repositories keep out of Git.
pub const HANDOVER_FILE: &str = ".toimija/handover.json";

/// What the last agent in this working copy left for the next one (`toimija
/// handover`), carried into every packet built here so the next agent reads
/// it on entering. A note, never a claim on work. It needs no session: an
/// agent started any way can write one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handover {
    /// Unix seconds it was written.
    pub written_at: u64,
    /// The session that wrote it, when its agent ran under `toimija run`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Branch and HEAD when it was written, so a reader can see how far the
    /// work has moved since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// What is not done, and where to pick it up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unfinished: Option<String>,
    /// What the next agent should know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// What caught the agent out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surprised: Option<String>,
}

/// Write `handover` as this working copy's, replacing the one before it.
pub fn write_handover(repo_root: &Path, handover: &Handover) -> Result<()> {
    let path = repo_root.join(HANDOVER_FILE);
    let dir = path.parent().expect("the handover file is under .toimija/");
    crate::owned::ensure_dir(repo_root, dir)?;
    let json = serde_json::to_string_pretty(handover)?;
    crate::owned::replace(repo_root, &path, format!("{json}\n").as_bytes())
        .context("failed to write the handover")
}

/// The handover left in this working copy, if any. One that exists and
/// cannot be read is warned about and left out, never fatal: a packet is
/// still built, and only a missing file is quiet.
pub fn read_handover(repo_root: &Path) -> Option<Handover> {
    let path = repo_root.join(HANDOVER_FILE);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            warn!(event = "toimija.handover.unreadable", path = %path.display(), error = %error);
            return None;
        }
    };
    match serde_json::from_str(&raw) {
        Ok(handover) => Some(handover),
        Err(error) => {
            warn!(event = "toimija.handover.unreadable", path = %path.display(), error = %error);
            None
        }
    }
}

/// Seconds since the Unix epoch; 0 if the system clock is before the epoch.
/// The fallback is warned about, not silent — a `0` here becomes a `0-<pid>`
/// session id and `started_at: 0`, which would otherwise read as mystery data.
pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_else(|error| {
            eprintln!(
                "toimija: system clock is before the Unix epoch ({error}); \
                 session timestamps fall back to 0"
            );
            0
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_marker(root: &Path, id: &str) -> SessionMarker {
        load_marker(&root.join(SESSIONS_SUBDIR).join(id)).unwrap()
    }

    #[test]
    fn session_id_is_timestamp_dash_pid() {
        assert_eq!(session_id(1782608449, 66987), "1782608449-66987");
    }

    #[test]
    fn session_id_validation_accepts_minted_ids_and_rejects_traversal() {
        assert!(is_valid_session_id("1782608449-66987"));
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id("no-dash-digits"));
        assert!(!is_valid_session_id("123-"));
        assert!(!is_valid_session_id("-123"));
        assert!(!is_valid_session_id("../../etc"));
        assert!(!is_valid_session_id("123-456/../../evil"));
    }

    #[test]
    fn declare_intent_updates_the_marker_and_appends_scope_deduped() {
        let temp = tempfile::tempdir().unwrap();
        let session = Session::open(
            temp.path(),
            &["claude".to_string()],
            None,
            vec!["src/a.rs".to_string()],
        )
        .unwrap();

        // A marker with no heartbeat, so the stamp is provable.
        let marker_path = session.dir.join("session.json");
        let mut unstamped = read_marker(temp.path(), &session.id);
        unstamped.last_seen_at = None;
        fs::write(
            &marker_path,
            serde_json::to_string_pretty(&unstamped).unwrap(),
        )
        .unwrap();

        let marker = declare_intent(
            temp.path(),
            &session.id,
            "closing tracker issues".to_string(),
            vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
        )
        .unwrap();
        assert_eq!(marker.intent.as_deref(), Some("closing tracker issues"));
        // Scope appends without duplicating the launch-time declaration.
        assert_eq!(marker.scope, vec!["src/a.rs", "src/b.rs"]);
        // Declaring intent is evidence of life: the heartbeat is stamped.
        assert!(marker.last_seen_at.unwrap() >= marker.started_at);
    }

    #[test]
    fn touch_stamps_last_seen_and_leaves_a_missing_marker_alone() {
        let temp = tempfile::tempdir().unwrap();
        let session =
            Session::open(temp.path(), &["claude".to_string()], None, Vec::new()).unwrap();
        let marker_path = session.dir.join("session.json");
        let mut unstamped = read_marker(temp.path(), &session.id);
        unstamped.last_seen_at = None;
        fs::write(
            &marker_path,
            serde_json::to_string_pretty(&unstamped).unwrap(),
        )
        .unwrap();

        touch(temp.path(), &session.id).unwrap();
        let stamped = read_marker(temp.path(), &session.id);
        assert!(stamped.last_seen_at.unwrap() >= stamped.started_at);

        // An ended session's marker is gone: nothing to stamp, nothing made.
        session.end().unwrap();
        touch(temp.path(), &stamped.id).unwrap();
        assert!(!temp.path().join(SESSIONS_SUBDIR).join(&stamped.id).exists());
        touch(temp.path(), "999-999").unwrap();
    }

    #[test]
    fn touch_surfaces_a_corrupt_marker_instead_of_silently_skipping() {
        let temp = tempfile::tempdir().unwrap();
        let session =
            Session::open(temp.path(), &["claude".to_string()], None, Vec::new()).unwrap();
        // Unreadable ≠ missing. A corrupt marker must error (`run` reports
        // it) — heartbeats silently stopping is how a live session gets
        // misreported stale.
        fs::write(session.dir.join("session.json"), "not json").unwrap();
        let error = touch(temp.path(), &session.id).unwrap_err();
        assert!(error.to_string().contains("failed to parse"));
    }

    #[test]
    fn end_removes_the_marker_and_returns_what_the_session_declared_while_it_ran() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = Session::open(
            temp.path(),
            &["claude".to_string()],
            Some("launch intent".to_string()),
            vec!["src/a.rs".to_string()],
        )
        .unwrap();
        session
            .record_git(Some("feature".to_string()), Some("abc123".to_string()))
            .unwrap();
        declare_intent(
            temp.path(),
            &session.id,
            "declared mid-session".to_string(),
            vec!["src/b.rs".to_string()],
        )
        .unwrap();
        let dir = session.dir.clone();

        let last = session.end().unwrap();

        assert!(!dir.exists(), "an ended session leaves no marker behind");
        assert_eq!(last.intent.as_deref(), Some("declared mid-session"));
        assert_eq!(last.scope, vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(last.branch.as_deref(), Some("feature"));
        assert_eq!(last.head.as_deref(), Some("abc123"));
    }

    #[test]
    fn open_writes_the_marker_with_the_agent_intent_scope_and_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let session = Session::open(
            temp.path(),
            &["claude".to_string(), "--flag".to_string()],
            Some("add sessions".to_string()),
            vec!["src/session.rs".to_string()],
        )
        .unwrap();

        let opened = read_marker(temp.path(), &session.id);
        assert_eq!(opened.pid, std::process::id());
        assert_eq!(opened.agent, vec!["claude", "--flag"]);
        assert_eq!(opened.branch, None);
        assert_eq!(opened.head, None);
        assert_eq!(opened.intent.as_deref(), Some("add sessions"));
        assert_eq!(opened.scope, vec!["src/session.rs"]);
        assert_eq!(opened.worktree, temp.path().display().to_string());
    }

    #[test]
    fn env_names_the_session_and_its_repository_and_no_packet_file() {
        let temp = tempfile::tempdir().unwrap();
        let session = Session::open(temp.path(), &["codex".to_string()], None, Vec::new()).unwrap();
        let env = session.env();
        let get = |key: &str| {
            env.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };

        assert_eq!(get("TOIMIJA_RUN").as_deref(), Some("1"));
        assert_eq!(get("TOIMIJA_SESSION"), Some(session.id.clone()));
        assert_eq!(get("TOIMIJA_RUN_ID"), Some(session.id.clone()));
        assert_eq!(
            get("TOIMIJA_REPO_ROOT"),
            Some(temp.path().display().to_string())
        );
        assert_eq!(env.len(), 4, "{env:?}");
    }
}
