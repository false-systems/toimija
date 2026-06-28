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
    pub branch: Option<String>,
    pub started_at: u64,
    pub status: SessionStatus,
    pub closed_at: Option<u64>,
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
    /// Open a new session under `repo_root` for the wrapped `agent` argv. Creates
    /// the session directory and writes the marker (status: active).
    pub fn open(repo_root: &Path, agent: &[String]) -> Result<Session> {
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
            branch: None,
            started_at,
            status: SessionStatus::Active,
            closed_at: None,
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

    /// Record the branch the session is working on (known after the first
    /// refresh), and rewrite the marker.
    pub fn set_branch(&mut self, branch: Option<String>) -> Result<()> {
        self.marker.branch = branch;
        self.write_marker()
    }

    /// Close the session: status -> closed, stamp `closed_at`, rewrite the marker.
    /// `run` calls this when the wrapped command exits (decision D4).
    pub fn close(&mut self) -> Result<()> {
        self.marker.status = SessionStatus::Closed;
        self.marker.closed_at = Some(now_secs());
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
        let mut session =
            Session::open(temp.path(), &["claude".to_string(), "--flag".to_string()]).unwrap();

        // The directory and marker exist, recording the agent argv as active.
        assert!(session.dir.is_dir());
        let raw = fs::read_to_string(session.dir.join("session.json")).unwrap();
        let opened: SessionMarker = serde_json::from_str(&raw).unwrap();
        assert_eq!(opened.status, SessionStatus::Active);
        assert_eq!(opened.agent, vec!["claude", "--flag"]);
        assert_eq!(opened.branch, None);
        assert!(opened.closed_at.is_none());

        session.set_branch(Some("feature".to_string())).unwrap();
        session.close().unwrap();

        let raw = fs::read_to_string(session.dir.join("session.json")).unwrap();
        let closed: SessionMarker = serde_json::from_str(&raw).unwrap();
        assert_eq!(closed.status, SessionStatus::Closed);
        assert_eq!(closed.branch.as_deref(), Some("feature"));
        assert!(closed.closed_at.is_some());
    }

    #[test]
    fn env_points_live_at_the_session_packet_and_repo_live_at_the_summary() {
        let temp = tempfile::tempdir().unwrap();
        let session = Session::open(temp.path(), &["codex".to_string()]).unwrap();
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
