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

use anyhow::{Context, Result, ensure};
use false_agent_protocol::{WorkerRunRef, WorkerRunResultEnvelope};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::refresh;
use crate::sauma::{self, SaumaEnvelope, SaumaProvenance};

/// Directory, relative to the repo root, holding all per-session state.
pub const SESSIONS_SUBDIR: &str = ".toimija/sessions";
pub const RUN_RECEIPT_SCHEMA: &str = "toimija-run-session.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunReceipt {
    pub schema: String,
    pub session: String,
    pub worker_run: WorkerRunRef,
    pub result_digest: String,
    pub worktree: String,
    pub closed_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sauma: Option<SaumaEnvelope>,
}

impl sauma::Carrier for RunReceipt {
    fn sauma_mut(&mut self) -> &mut Option<SaumaEnvelope> {
        &mut self.sauma
    }
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
            last_seen_at: Some(started_at),
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

    /// Record the entry branch and HEAD once they are known after the first
    /// refresh, and rewrite the marker. Later refreshes do not overwrite them.
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

    pub fn record_worker_run(
        &self,
        repo_root: &Path,
        result: &WorkerRunResultEnvelope,
    ) -> Result<PathBuf> {
        ensure!(
            self.marker.status == SessionStatus::Closed,
            "cannot record a worker run for active session {}",
            self.id
        );
        let closed_at = self
            .marker
            .closed_at
            .context("closed session has no close timestamp")?;
        let mut receipt = RunReceipt {
            schema: RUN_RECEIPT_SCHEMA.into(),
            session: self.id.clone(),
            worker_run: result.run.clone(),
            result_digest: result.result_digest.clone(),
            worktree: self.marker.worktree.clone(),
            closed_at,
            sauma: None,
        };
        sauma::attach(
            &mut receipt,
            RUN_RECEIPT_SCHEMA,
            vec![
                SaumaProvenance::Session {
                    id: self.id.clone(),
                },
                SaumaProvenance::WorkerRun {
                    id: result.run.to_string(),
                },
                SaumaProvenance::Artifact {
                    digest: result.result_digest.clone(),
                },
            ],
            &result.result_digest,
            Some(self.id.clone()),
        )?;
        let path = run_receipt_path(repo_root, &result.run);
        if path.exists() {
            let existing = read_run_receipt(repo_root, &result.run)?;
            ensure!(
                existing == receipt,
                "worker run {} is already owned by session {}",
                result.run,
                existing.session
            );
            return Ok(path);
        }
        let mut bytes = serde_json::to_vec_pretty(&receipt)?;
        bytes.push(b'\n');
        crate::atomic_write::replace(&path, &bytes)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(path)
    }

    /// The live-packet handshake the wrapped child inherits. The per-session
    /// `TOIMIJA_LIVE` is authoritative for the agent; `TOIMIJA_REPO_LIVE` is the
    /// demoted repo-level summary. Absolute paths, so they survive the agent
    /// changing directory mid-session.
    pub fn env(&self, repo_root: &Path) -> Vec<(&'static str, OsString)> {
        let mut env = vec![
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
                "FALSE_SYSTEMS_RESULT_PATH",
                self.dir.join(crate::semantic::RESULT_FILE).into_os_string(),
            ),
            (
                "FALSE_SYSTEMS_REPOSITORY_POINT_PATH",
                self.dir.join(crate::semantic::POINT_FILE).into_os_string(),
            ),
        ];
        let context = self.dir.join(crate::semantic::CONTEXT_FILE);
        if context.is_file() {
            env.push(("FALSE_SYSTEMS_CONTEXT_PATH", context.into_os_string()));
        }
        env
    }

    fn write_marker(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.marker)?;
        crate::atomic_write::replace(&self.marker_path, format!("{json}\n").as_bytes())
            .with_context(|| format!("failed to write {}", self.marker_path.display()))?;
        Ok(())
    }
}

pub fn run_receipt_path(repo_root: &Path, run: &WorkerRunRef) -> PathBuf {
    repo_root
        .join(".toimija/runs")
        .join(format!("{run}.session.json"))
}

pub fn read_run_receipt(repo_root: &Path, run: &WorkerRunRef) -> Result<RunReceipt> {
    const MAX_BYTES: u64 = 1024 * 1024;
    let path = run_receipt_path(repo_root, run);
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= MAX_BYTES,
        "run receipt must be a bounded regular file"
    );
    let mut receipt: RunReceipt = serde_json::from_slice(&fs::read(&path)?)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    ensure!(
        receipt.schema == RUN_RECEIPT_SCHEMA,
        "unsupported run receipt schema"
    );
    ensure!(
        receipt
            .sauma
            .as_ref()
            .is_some_and(|envelope| envelope.envelope == sauma::ENVELOPE_SCHEMA),
        "run receipt has no supported Sauma envelope"
    );
    ensure!(
        sauma::verify(&mut receipt)?.is_none(),
        "run receipt digest mismatch"
    );
    Ok(receipt)
}

/// The `run` session this process was launched under, if any: a valid
/// `TOIMIJA_SESSION` id whose marker exists under *this* repo root and is
/// still active. Resolution is per-repo — an id inherited from a session in
/// another repo finds no marker here and resolves to `None`, so a handshake
/// can never steer writes into the wrong repo. Returns the id and the
/// session directory.
///
/// Unreadable ≠ missing (issue #161): a marker that exists but cannot be read
/// or parsed still resolves to `None` — writes must not target a session whose
/// state is unknown — but it is warned about, because the wrapped agent keeps
/// reading the per-session packet that will now silently stop refreshing.
pub fn ambient_session(repo_root: &Path) -> Option<(String, PathBuf)> {
    let id = std::env::var("TOIMIJA_SESSION").ok()?;
    if !is_valid_session_id(&id) {
        return None;
    }
    let dir = repo_root.join(SESSIONS_SUBDIR).join(&id);
    let marker_path = dir.join("session.json");
    let raw = match fs::read_to_string(&marker_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            warn!(
                event = "toimija.session.marker_unreadable",
                path = %marker_path.display(),
                error = %error
            );
            return None;
        }
    };
    let marker: SessionMarker = match serde_json::from_str(&raw) {
        Ok(marker) => marker,
        Err(error) => {
            warn!(
                event = "toimija.session.marker_unreadable",
                path = %marker_path.display(),
                error = %error
            );
            return None;
        }
    };
    (marker.id == id && marker.status == SessionStatus::Active).then_some((id, dir))
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
    stamp(&marker_path, &marker)
}

/// Write half of the heartbeat, split out so the prune race is testable
/// (issue #159): the session directory can vanish between `touch`'s read and
/// this write when a sibling's startup prune wins the interleaving. `NotFound`
/// then means "pruned — nothing left to stamp", the same quiet outcome as a
/// marker already missing at the read. Every other error still surfaces.
fn stamp(marker_path: &Path, marker: &SessionMarker) -> Result<()> {
    let json = serde_json::to_string_pretty(marker)?;
    match crate::atomic_write::replace(marker_path, format!("{json}\n").as_bytes()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to write {}", marker_path.display()))
        }
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
    crate::atomic_write::replace(&marker_path, format!("{json}\n").as_bytes())
        .with_context(|| format!("failed to write {}", marker_path.display()))?;
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
        stamp(&dir.join("session.json"), &marker).unwrap();
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
