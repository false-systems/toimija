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

use crate::refresh;

/// Directory, relative to the repo root, holding all per-session state.
pub const SESSIONS_SUBDIR: &str = ".toimija/sessions";

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
    pub worktree: String,
    pub branch: Option<String>,
    /// HEAD commit at the latest refresh. Known after the first packet build,
    /// like `branch`; `None` until then.
    pub head: Option<String>,
    /// Free-text declaration of *why* the session is here. `None` renders as
    /// `unknown` — never an error, but awareness may warn when an unknown-intent
    /// session shares a branch with another. Advisory, not an assignment.
    pub intent: Option<String>,
    /// Zero or more declared paths the session expects to touch — *what* it might
    /// touch. Advisory only: never a lock, never ownership. Empty renders as
    /// `unknown`.
    pub scope: Vec<String>,
    pub started_at: u64,
    pub status: SessionStatus,
    pub closed_at: Option<u64>,
    /// Wrapped command's exit code, recorded on close when known.
    pub exit_status: Option<i32>,
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
        let started_at = now_secs();
        let pid = std::process::id();
        let id = session_id(started_at, pid);
        let dir = repo_root.join(SESSIONS_SUBDIR).join(&id);
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create session dir {}", dir.display()))?;
        let marker = SessionMarker {
            id: id.clone(),
            pid,
            agent: agent.to_vec(),
            worktree: repo_root.display().to_string(),
            branch: None,
            head: None,
            intent,
            scope,
            started_at,
            status: SessionStatus::Active,
            closed_at: None,
            exit_status: None,
        };
        let session = Session {
            id,
            md_path: dir.join("current.md"),
            json_path: dir.join("current.json"),
            marker_path: dir.join("session.json"),
            dir,
            marker,
        };
        session.write_marker()?;
        Ok(session)
    }

    /// Record the branch and HEAD the session is working on (both known after the
    /// first refresh), and rewrite the marker.
    pub fn record_git(&mut self, branch: Option<String>, head: Option<String>) -> Result<()> {
        self.marker.branch = branch;
        self.marker.head = head;
        self.write_marker()
    }

    /// Close the session: status -> closed, stamp `closed_at` and the wrapped
    /// command's `exit_status` (when known), rewrite the marker. `run` calls this
    /// when the wrapped command exits (decision D4).
    pub fn close(&mut self, exit_status: Option<i32>) -> Result<()> {
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
            (
                "TOIMIJA_REPO_LIVE",
                repo_root.join(refresh::CURRENT_MD).into_os_string(),
            ),
        ]
    }

    fn write_marker(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.marker)?;
        fs::write(&self.marker_path, format!("{json}\n"))
            .with_context(|| format!("failed to write {}", self.marker_path.display()))?;
        Ok(())
    }
}

/// Seconds since the Unix epoch; 0 if the system clock is before the epoch.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_is_timestamp_dash_pid() {
        assert_eq!(session_id(1782608449, 66987), "1782608449-66987");
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
