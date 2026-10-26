//! Per-session live packets and markers for `toimija run`.
//!
//! Each `run` session owns `.toimija/sessions/<id>/`, holding its own live packet
//! (`current.md`/`current.json`) and a `session.json` marker. This isolates
//! concurrent wrapped agents, which would otherwise race on the single
//! repo-level `.toimija/current.*`. No daemon, no network: the marker is a plain
//! file written when the session opens and rewritten when it closes.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::refresh;

/// Directory, relative to the repo root, holding all per-session state.
pub const SESSIONS_SUBDIR: &str = ".toimija/sessions";

/// Handshake variables earlier versions set and this one does not: the
/// worker.v1 result and context paths. `run` clears them for the child, so a
/// value inherited from an outer session cannot point it at a stale file.
pub const RETIRED_ENV: &[&str] = &["FALSE_SYSTEMS_RESULT_PATH", "FALSE_SYSTEMS_CONTEXT_PATH"];

/// Handshake variables that only mean something inside a repository. A
/// session outside one clears them for the child, so values inherited from an
/// outer session cannot point it at another session's repo or packet.
pub const REPO_ONLY_ENV: &[&str] = &[
    "TOIMIJA_LIVE_JSON",
    "TOIMIJA_CURRENT_JSON",
    "TOIMIJA_REPO_ROOT",
    "TOIMIJA_REPO_LIVE",
    "FALSE_SYSTEMS_REPOSITORY_POINT_PATH",
];

/// Overrides where sessions opened outside any Git repository live.
pub const STATE_DIR_ENV: &str = "TOIMIJA_STATE_DIR";

/// Where sessions opened outside any Git repository live (issue #225):
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

/// Lifecycle of a session, recorded in its marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Closed,
}

/// On-disk session marker (`session.json`). Self-describing so later layers
/// (`toimija sessions`, `toimija handoff`) can read it without extra context —
/// and so the cross-repo north star is not blocked by an under-specified record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMarker {
    pub id: String,
    pub pid: u32,
    pub agent: Vec<String>,
    /// Absolute path of the worktree the session runs in. Distinguishes sessions
    /// that share a repo but check out separate worktrees — the preferred
    /// isolation for parallel agents (same codebase, separate sessions).
    ///
    /// `#[serde(default)]` (here and on the fields below) keeps the *reader*
    /// tolerant of markers written before these fields existed: a missing field
    /// reads as its default rather than failing the whole record. Additivity has
    /// two directions — old readers ignore new fields; new readers must not choke
    /// on old records.
    #[serde(default)]
    pub worktree: String,
    pub branch: Option<String>,
    /// HEAD commit captured at session entry. Known after the first packet
    /// build, like `branch`; it stays fixed so later packets can report drift.
    #[serde(default)]
    pub head: Option<String>,
    /// Free-text declaration of *why* the session is here. `None` renders as
    /// `unknown` — never an error, but awareness may warn when an unknown-intent
    /// session shares a branch with another. Advisory, not an assignment.
    #[serde(default)]
    pub intent: Option<String>,
    /// Zero or more declared paths the session expects to touch — *what* it might
    /// touch. Advisory only: never a lock, never ownership. Empty renders as
    /// `unknown`.
    #[serde(default)]
    pub scope: Vec<String>,
    pub started_at: u64,
    /// Unix seconds of the last observed activity — stamped by `run`'s watcher
    /// on each packet refresh (proposal 0002). Staleness measures inactivity
    /// from here, not lifetime from `started_at`. `None` (a marker written
    /// before the field existed) falls back to `started_at`.
    #[serde(default)]
    pub last_seen_at: Option<u64>,
    pub status: SessionStatus,
    pub closed_at: Option<u64>,
    /// Wrapped command's exit code, recorded on close when known.
    #[serde(default)]
    pub exit_status: Option<i32>,
    /// What `run` had for the agent but could not put in its context, because
    /// it has no delivery for that agent: `packet`, `task`, `briefing`. Empty
    /// when everything was delivered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undelivered: Vec<String>,
}

/// Compose a session id as `<unix-secs>-<pid>` (decision D1). Roughly sortable by
/// start time; the pid disambiguates sessions opened in the same second. Pure, so
/// tests are deterministic.
pub fn session_id(started_at: u64, pid: u32) -> String {
    format!("{started_at}-{pid}")
}

/// An open `run` session: its directory, packet paths, and marker.
pub struct Session {
    pub id: String,
    repo_root: PathBuf,
    pub dir: PathBuf,
    pub md_path: PathBuf,
    pub json_path: PathBuf,
    marker_path: PathBuf,
    marker: SessionMarker,
}

impl Session {
    /// Open a new session under `repo_root` for the wrapped `agent` argv,
    /// carrying the optional `intent` and declared `scope`. Creates the session
    /// directory and writes the marker (status: active).
    pub fn open(
        repo_root: &Path,
        agent: &[String],
        intent: Option<String>,
        scope: Vec<String>,
    ) -> Result<Session> {
        Self::open_at(repo_root, repo_root, agent, intent, scope)
    }

    /// Open a session for an agent working in `folder`, which is not inside
    /// any Git repository (issue #225). The session lives under `state_root`
    /// (see [`outside_root`]) and records `folder` as its worktree. It has no
    /// context packet: `current.md` only says why, so the listing's packet path
    /// is never a file that does not exist.
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
        let session = Self::open_at(state_root, folder, agent, intent, scope)?;
        let note = format!(
            "# Toimija session outside a repository\n\n\
             - session: {}\n\
             - folder: {}\n\n\
             This folder is not inside a Git repository, so there is no Git context to \
             compile and this file is not refreshed.\n",
            session.id,
            folder.display()
        );
        crate::owned::replace(state_root, &session.md_path, note.as_bytes())?;
        Ok(session)
    }

    /// Open a session under `repo_root` whose agent works in `worktree`.
    fn open_at(
        repo_root: &Path,
        worktree: &Path,
        agent: &[String],
        intent: Option<String>,
        scope: Vec<String>,
    ) -> Result<Session> {
        let started_at = now_secs();
        let pid = std::process::id();
        let id = session_id(started_at, pid);
        let dir = repo_root.join(SESSIONS_SUBDIR).join(&id);
        crate::owned::ensure_dir(repo_root, &dir).context("failed to open the session")?;
        let marker = SessionMarker {
            id: id.clone(),
            pid,
            agent: agent.to_vec(),
            worktree: worktree.display().to_string(),
            branch: None,
            head: None,
            intent,
            scope,
            started_at,
            last_seen_at: Some(started_at),
            status: SessionStatus::Active,
            closed_at: None,
            exit_status: None,
            undelivered: Vec::new(),
        };
        let session = Session {
            id,
            repo_root: repo_root.to_path_buf(),
            md_path: dir.join("current.md"),
            json_path: dir.join("current.json"),
            marker_path: dir.join("session.json"),
            dir,
            marker,
        };
        session.write_marker()?;
        Ok(session)
    }

    /// Record the entry branch and HEAD once they are known after the first
    /// refresh, and rewrite the marker. Later refreshes do not overwrite them.
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

    /// Close the session: status -> closed, stamp `closed_at` and the wrapped
    /// command's `exit_status` (when known), rewrite the marker. `run` calls this
    /// when the wrapped command exits (decision D4).
    ///
    /// The wrapped agent may have rewritten the marker on disk while it ran —
    /// `toimija intent` declares intent and scope, heartbeats stamp
    /// `last_seen_at` — so those are taken from disk first. Writing the copy
    /// held since entry would erase what the session declared about itself.
    pub fn close(&mut self, exit_status: Option<i32>) -> Result<()> {
        match load_marker(&self.dir) {
            Ok(on_disk) if on_disk.id == self.marker.id => {
                self.marker.intent = on_disk.intent;
                self.marker.scope = on_disk.scope;
                self.marker.last_seen_at = self.marker.last_seen_at.max(on_disk.last_seen_at);
            }
            // Unreadable or replaced: close with what entry knew, and say so.
            Ok(_) | Err(_) => warn!(
                event = "toimija.session.close_without_declarations",
                session = %self.id,
                "the marker on disk could not be read back; declared intent and scope may be lost"
            ),
        }
        self.marker.status = SessionStatus::Closed;
        self.marker.closed_at = Some(now_secs());
        self.marker.exit_status = exit_status;
        self.write_marker()
    }

    /// The live-packet handshake the wrapped child inherits. The per-session
    /// `TOIMIJA_LIVE` is authoritative for the agent; `TOIMIJA_REPO_LIVE` is the
    /// demoted repo-level summary. Absolute paths, so they survive the agent
    /// changing directory mid-session.
    pub fn env(&self, repo_root: &Path) -> Vec<(&'static str, OsString)> {
        vec![
            ("TOIMIJA_RUN", OsString::from("1")),
            ("TOIMIJA_SESSION", OsString::from(&self.id)),
            ("TOIMIJA_LIVE", self.md_path.clone().into_os_string()),
            ("TOIMIJA_LIVE_JSON", self.json_path.clone().into_os_string()),
            ("TOIMIJA_CURRENT_MD", self.md_path.clone().into_os_string()),
            (
                "TOIMIJA_CURRENT_JSON",
                self.json_path.clone().into_os_string(),
            ),
            ("TOIMIJA_REPO_ROOT", repo_root.as_os_str().to_os_string()),
            ("TOIMIJA_RUN_ID", OsString::from(&self.id)),
            (
                "TOIMIJA_REPO_LIVE",
                repo_root.join(refresh::CURRENT_MD).into_os_string(),
            ),
            (
                "FALSE_SYSTEMS_REPOSITORY_POINT_PATH",
                self.dir.join(crate::semantic::POINT_FILE).into_os_string(),
            ),
        ]
    }

    /// The handshake for a session outside any repository: who it is, and the
    /// note standing in for a packet. Pair with clearing [`REPO_ONLY_ENV`].
    pub fn env_outside(&self) -> Vec<(&'static str, OsString)> {
        vec![
            (STATE_DIR_ENV, self.repo_root.clone().into_os_string()),
            ("TOIMIJA_RUN", OsString::from("1")),
            ("TOIMIJA_SESSION", OsString::from(&self.id)),
            ("TOIMIJA_RUN_ID", OsString::from(&self.id)),
            ("TOIMIJA_LIVE", self.md_path.clone().into_os_string()),
            ("TOIMIJA_CURRENT_MD", self.md_path.clone().into_os_string()),
        ]
    }

    fn write_marker(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.marker)?;
        crate::owned::replace(
            &self.repo_root,
            &self.marker_path,
            format!("{json}\n").as_bytes(),
        )
    }
}

/// The `run` session this process was launched under, if any: a valid
/// `TOIMIJA_SESSION` id whose marker exists under *this* repo root and is
/// still active. Resolution is per-repo — an id inherited from a session in
/// another repo finds no marker here and resolves to `None`, so a handshake
/// can never steer writes into the wrong repo. Returns the id and the
/// session directory.
///
/// Unreadable ≠ missing (issue #161): a marker that exists but cannot be read
/// or parsed — or whose recorded id disagrees with its directory — still
/// resolves to `None` (writes must not target a session whose state is
/// unknown), but it is warned about, because the wrapped agent keeps reading
/// the per-session packet that will now silently stop refreshing. Only a
/// missing marker and a closed session stay quiet. A marker still active
/// whose process is provably gone resolves to `None` too, like a closed one.
pub fn ambient_session(repo_root: &Path) -> Option<(String, PathBuf)> {
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
    if marker.status != SessionStatus::Active {
        return None;
    }
    // A `run` killed before it could close its marker (SIGKILL, OOM) leaves
    // the agent running with the handshake. That session is over: treat it
    // like a closed one, so a refresh writes a packet without its brief.
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

/// Read the marker in a session directory. Used by the per-session refresh
/// writer to carry the session's identity (intent, scope) into the packet.
pub fn load_marker(session_dir: &Path) -> Result<SessionMarker> {
    let marker_path = session_dir.join("session.json");
    let raw = fs::read_to_string(&marker_path)
        .with_context(|| format!("failed to read {}", marker_path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", marker_path.display()))
}

/// Best-effort activity heartbeat (proposal 0002): stamp `last_seen_at = now`
/// on an open marker. Called by `run`'s watcher after each packet refresh —
/// an event worth a refresh is worth a heartbeat, and nothing else is. A
/// closed or missing marker is left alone: the heartbeat must never fight
/// the close path. An unreadable or corrupt marker is an error (issue #161)
/// — callers log it and keep looping, but silence here means heartbeats stop
/// stamping and a live session gets misreported stale.
pub fn touch(repo_root: &Path, id: &str) -> Result<()> {
    let marker_path = repo_root
        .join(SESSIONS_SUBDIR)
        .join(id)
        .join("session.json");
    let raw = match fs::read_to_string(&marker_path) {
        Ok(raw) => raw,
        // Marker gone (pruned, closed early) — nothing to stamp.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", marker_path.display()));
        }
    };
    let mut marker: SessionMarker = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", marker_path.display()))?;
    if marker.status != SessionStatus::Active {
        return Ok(());
    }
    marker.last_seen_at = Some(now_secs());
    stamp(repo_root, &marker_path, &marker)
}

/// Write half of the heartbeat, split out so the prune race is testable
/// (issue #159): the session directory can vanish between `touch`'s read and
/// this write when a sibling's startup prune wins the interleaving. `NotFound`
/// then means "pruned — nothing left to stamp", the same quiet outcome as a
/// marker already missing at the read. Every other error still surfaces.
fn stamp(repo_root: &Path, marker_path: &Path, marker: &SessionMarker) -> Result<()> {
    let json = serde_json::to_string_pretty(marker)?;
    match crate::owned::replace(repo_root, marker_path, format!("{json}\n").as_bytes()) {
        Ok(()) => Ok(()),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Whether `id` has the `<unix-secs>-<pid>` shape `run` mints (decision D1).
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

/// Update the intent (and append declared scope paths) on an *existing* session
/// marker — the session belongs to `run`; this is the wrapped agent reaching
/// back into its own record via the `TOIMIJA_SESSION` handshake. Advisory data
/// only: never an assignment, never a lock. Returns the updated marker.
pub fn declare_intent(
    repo_root: &Path,
    id: &str,
    intent: String,
    scope: Vec<String>,
) -> Result<SessionMarker> {
    anyhow::ensure!(
        is_valid_session_id(id),
        "invalid session id `{id}` — expected the `<unix-secs>-<pid>` shape from TOIMIJA_SESSION"
    );
    let marker_path = repo_root
        .join(SESSIONS_SUBDIR)
        .join(id)
        .join("session.json");
    let raw = fs::read_to_string(&marker_path).with_context(|| {
        format!(
            "no session marker at {} — is the session still open?",
            marker_path.display()
        )
    })?;
    let mut marker: SessionMarker = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", marker_path.display()))?;
    anyhow::ensure!(
        marker.status == SessionStatus::Active,
        "session {id} is closed; intent can only be declared on a live session"
    );
    marker.intent = Some(intent);
    for path in scope {
        if !marker.scope.contains(&path) {
            marker.scope.push(path);
        }
    }
    // Declaring intent is evidence of life: stamp the heartbeat too, so a
    // session that speaks is never simultaneously reported stale.
    marker.last_seen_at = Some(now_secs());
    let json = serde_json::to_string_pretty(&marker)?;
    crate::owned::replace(repo_root, &marker_path, format!("{json}\n").as_bytes())
        .context("failed to write the session marker")?;
    Ok(marker)
}

/// Seconds since the Unix epoch; 0 if the system clock is before the epoch.
/// The fallback is warned about, not silent — a `0` here becomes a `0-<pid>`
/// session id and `started_at: 0`, which would otherwise read as mystery data.
fn now_secs() -> u64 {
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
    fn declare_intent_updates_a_live_marker_and_appends_scope_deduped() {
        let temp = tempfile::tempdir().unwrap();
        let session = Session::open(
            temp.path(),
            &["claude".to_string()],
            None,
            vec!["src/a.rs".to_string()],
        )
        .unwrap();

        // Simulate a pre-0002 marker (no heartbeat) so the stamp is provable.
        let marker_path = temp
            .path()
            .join(SESSIONS_SUBDIR)
            .join(&session.id)
            .join("session.json");
        let mut stale: SessionMarker =
            serde_json::from_str(&fs::read_to_string(&marker_path).unwrap()).unwrap();
        stale.last_seen_at = None;
        fs::write(&marker_path, serde_json::to_string_pretty(&stale).unwrap()).unwrap();

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
    fn touch_stamps_last_seen_on_a_live_marker_only() {
        let temp = tempfile::tempdir().unwrap();
        let mut session =
            Session::open(temp.path(), &["claude".to_string()], None, Vec::new()).unwrap();
        let read_marker = |id: &str| -> SessionMarker {
            let raw = fs::read_to_string(
                temp.path()
                    .join(SESSIONS_SUBDIR)
                    .join(id)
                    .join("session.json"),
            )
            .unwrap();
            serde_json::from_str(&raw).unwrap()
        };

        // Open marker: heartbeat stamps (>= the open-time stamp; the clock
        // may not tick between open and touch).
        touch(temp.path(), &session.id).unwrap();
        let stamped = read_marker(&session.id);
        assert!(stamped.last_seen_at.unwrap() >= stamped.started_at);

        // Closed marker: heartbeat leaves it alone (the watcher may fire
        // around the close; it must not fight the close path).
        session.close(Some(0)).unwrap();
        let closed = read_marker(&session.id);
        touch(temp.path(), &session.id).unwrap();
        assert_eq!(read_marker(&session.id), closed);

        // Missing marker: quietly Ok — never interrupts the watch loop.
        touch(temp.path(), "999-999").unwrap();
    }

    #[test]
    fn touch_surfaces_a_corrupt_marker_instead_of_silently_skipping() {
        let temp = tempfile::tempdir().unwrap();
        let session =
            Session::open(temp.path(), &["claude".to_string()], None, Vec::new()).unwrap();
        let marker_path = temp
            .path()
            .join(SESSIONS_SUBDIR)
            .join(&session.id)
            .join("session.json");
        // Issue #161: unreadable ≠ missing. A corrupt marker must error (the
        // watcher logs it) — heartbeats silently stopping is how a live
        // session gets misreported stale.
        fs::write(&marker_path, "not json").unwrap();
        let error = touch(temp.path(), &session.id).unwrap_err();
        assert!(error.to_string().contains("failed to parse"));
    }

    #[test]
    fn stamp_is_quiet_when_prune_removed_the_session_dir_mid_heartbeat() {
        let temp = tempfile::tempdir().unwrap();
        let session =
            Session::open(temp.path(), &["claude".to_string()], None, Vec::new()).unwrap();
        let dir = temp.path().join(SESSIONS_SUBDIR).join(&session.id);
        let marker = load_marker(&dir).unwrap();
        // The exact interleaving from issue #159: marker read, prune removes
        // the directory, then the heartbeat writes. The write must be the
        // quiet outcome, not `failed to write …/session.json.tmp-…`.
        fs::remove_dir_all(&dir).unwrap();
        stamp(temp.path(), &dir.join("session.json"), &marker).unwrap();
        assert!(!dir.exists(), "a quiet stamp must not resurrect the dir");
    }

    #[test]
    fn declare_intent_refuses_a_closed_session() {
        let temp = tempfile::tempdir().unwrap();
        let mut session =
            Session::open(temp.path(), &["claude".to_string()], None, Vec::new()).unwrap();
        session.close(Some(0)).unwrap();

        let error = declare_intent(temp.path(), &session.id, "too late".to_string(), Vec::new())
            .unwrap_err();
        assert!(error.to_string().contains("closed"));
    }

    #[test]
    fn close_keeps_the_intent_and_scope_the_session_declared_while_it_ran() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = Session::open(
            temp.path(),
            &["claude".to_string()],
            Some("launch intent".to_string()),
            vec!["src/a.rs".to_string()],
        )
        .unwrap();
        declare_intent(
            temp.path(),
            &session.id,
            "declared mid-session".to_string(),
            vec!["src/b.rs".to_string()],
        )
        .unwrap();

        session.close(Some(3)).unwrap();

        let closed = load_marker(&session.dir).unwrap();
        assert_eq!(closed.status, SessionStatus::Closed);
        assert_eq!(closed.exit_status, Some(3));
        assert_eq!(closed.intent.as_deref(), Some("declared mid-session"));
        assert_eq!(closed.scope, vec!["src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn open_writes_an_active_marker_then_close_marks_it_closed() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = Session::open(
            temp.path(),
            &["claude".to_string(), "--flag".to_string()],
            Some("add sessions".to_string()),
            vec!["src/session.rs".to_string()],
        )
        .unwrap();

        // The directory and marker exist, recording the agent argv as active,
        // along with the declared intent and scope and the worktree path.
        assert!(session.dir.is_dir());
        let raw = fs::read_to_string(session.dir.join("session.json")).unwrap();
        let opened: SessionMarker = serde_json::from_str(&raw).unwrap();
        assert_eq!(opened.status, SessionStatus::Active);
        assert_eq!(opened.agent, vec!["claude", "--flag"]);
        assert_eq!(opened.branch, None);
        assert_eq!(opened.head, None);
        assert_eq!(opened.intent.as_deref(), Some("add sessions"));
        assert_eq!(opened.scope, vec!["src/session.rs"]);
        assert_eq!(opened.worktree, temp.path().display().to_string());
        assert!(opened.closed_at.is_none());
        assert!(opened.exit_status.is_none());

        session
            .record_git(Some("feature".to_string()), Some("abc123".to_string()))
            .unwrap();
        session.close(Some(0)).unwrap();

        let raw = fs::read_to_string(session.dir.join("session.json")).unwrap();
        let closed: SessionMarker = serde_json::from_str(&raw).unwrap();
        assert_eq!(closed.status, SessionStatus::Closed);
        assert_eq!(closed.branch.as_deref(), Some("feature"));
        assert_eq!(closed.head.as_deref(), Some("abc123"));
        assert_eq!(closed.exit_status, Some(0));
        assert!(closed.closed_at.is_some());
    }

    #[test]
    fn reads_an_old_marker_that_predates_the_added_fields() {
        // A Layer 1 marker, before worktree/head/intent/scope/exit_status existed.
        let old = r#"{
            "id": "1-2",
            "pid": 2,
            "agent": ["codex"],
            "branch": "feat/x",
            "started_at": 100,
            "status": "active",
            "closed_at": null
        }"#;
        let marker: SessionMarker = serde_json::from_str(old).unwrap();
        assert_eq!(marker.agent, vec!["codex"]);
        assert_eq!(marker.worktree, "");
        assert_eq!(marker.intent, None);
        assert!(marker.scope.is_empty());
        assert_eq!(marker.exit_status, None);
    }

    #[test]
    fn env_points_live_at_the_session_packet_and_repo_live_at_the_summary() {
        let temp = tempfile::tempdir().unwrap();
        let session = Session::open(temp.path(), &["codex".to_string()], None, Vec::new()).unwrap();
        let env = session.env(temp.path());
        let get = |key: &str| {
            env.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string_lossy().into_owned())
                .unwrap_or_default()
        };

        assert_eq!(get("TOIMIJA_RUN"), "1");
        assert_eq!(get("TOIMIJA_SESSION"), session.id);
        // Authoritative per-session packet lives under the session dir.
        let live = get("TOIMIJA_LIVE");
        assert!(live.contains(".toimija/sessions/"), "{live}");
        assert!(live.ends_with("current.md"), "{live}");
        // Repo-level summary is the demoted, shared current.md.
        assert!(get("TOIMIJA_REPO_LIVE").ends_with(".toimija/current.md"));
    }
}
