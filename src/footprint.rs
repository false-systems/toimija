//! Ownership and durable history for files under `.toimija/`.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::atomic_write::TEMP_MARKER;
use crate::handoff;
use crate::session::SessionMarker;

pub const HISTORY_PATH: &str = ".toimija/history.jsonl";
const HISTORY_VERSION: u32 = 1;
const HANDOFF_CAP: usize = 10;

/// Every artifact class Toimija owns under `.toimija/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactClass {
    Config,
    LivePacket,
    SessionDir,
    RunTranscript,
    SnapshotLease,
    TempFile,
    HistoryJournal,
}

impl ArtifactClass {
    pub fn owner(self) -> &'static str {
        match self {
            Self::Config => "human",
            Self::LivePacket => "enter/refresh",
            Self::SessionDir => "run",
            Self::RunTranscript => "run harness",
            Self::SnapshotLease => "snapshot",
            Self::TempFile => "atomic write",
            Self::HistoryJournal => "footprint stewardship",
        }
    }

    pub fn retention(self) -> &'static str {
        match self {
            Self::Config => "permanent",
            Self::LivePacket => "overwritten in place",
            Self::SessionDir => "closed or stale plus retention window",
            Self::RunTranscript => "retention window",
            Self::SnapshotLease => "lease scoped",
            Self::TempFile => "one hour after orphaning",
            Self::HistoryJournal => "bounded compaction",
        }
    }

    pub fn contains(self, path: &Path) -> bool {
        classify(path) == Some(self)
    }
}

/// Classify one repo-relative path under `.toimija/`.
pub fn classify(path: &Path) -> Option<ArtifactClass> {
    let path = path.strip_prefix("./").unwrap_or(path);
    if !path.starts_with(".toimija") {
        return None;
    }
    if is_atomic_temp(path) {
        return Some(ArtifactClass::TempFile);
    }
    if path == Path::new(".toimija/repo.yaml") {
        return Some(ArtifactClass::Config);
    }
    if path == Path::new(".toimija/current.md") || path == Path::new(".toimija/current.json") {
        return Some(ArtifactClass::LivePacket);
    }
    if path == Path::new(HISTORY_PATH) {
        return Some(ArtifactClass::HistoryJournal);
    }
    if path.starts_with(".toimija/sessions") {
        return Some(ArtifactClass::SessionDir);
    }
    if path.starts_with(".toimija/runs") {
        return Some(ArtifactClass::RunTranscript);
    }
    if path.starts_with(".toimija/snapshots") {
        return Some(ArtifactClass::SnapshotLease);
    }
    None
}

fn is_atomic_temp(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some((_, suffix)) = name.rsplit_once(TEMP_MARKER) else {
        return false;
    };
    let Some((pid, sequence)) = suffix.split_once('-') else {
        return false;
    };
    !pid.is_empty()
        && !sequence.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && sequence.bytes().all(|byte| byte.is_ascii_digit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryEnd {
    Closed,
    ReapedStale,
}

impl HistoryEnd {
    fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::ReapedStale => "reaped_stale",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub version: u32,
    pub id: String,
    pub agent: Vec<String>,
    pub intent: Option<String>,
    pub scope: Vec<String>,
    pub branch: Option<String>,
    pub started_at: u64,
    pub last_seen_at: Option<u64>,
    pub end: HistoryEnd,
    pub exit_status: Option<i32>,
    pub handoffs: Vec<String>,
    pub handoffs_total: usize,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRead {
    pub records: Vec<HistoryRecord>,
    pub malformed: usize,
}

/// An exclusively locked journal and its one-pass record index.
pub struct Journal {
    path: PathBuf,
    file: fs::File,
    recorded: BTreeSet<String>,
}

impl Journal {
    pub fn open(repo_root: &Path) -> Result<Self> {
        let (path, mut file) = open_history(repo_root)?;
        file.lock()
            .with_context(|| format!("failed to lock {}", path.display()))?;
        let recorded = read_locked(&mut file, &path)?
            .records
            .into_iter()
            .map(|record| record.id)
            .collect();
        Ok(Self {
            path,
            file,
            recorded,
        })
    }

    pub fn journal_session(
        &mut self,
        repo_root: &Path,
        marker: &SessionMarker,
        end: HistoryEnd,
        recorded_at: u64,
    ) -> Result<bool> {
        if self.recorded.contains(&marker.id) {
            return Ok(false);
        }
        let handoff_path = repo_root
            .join(".toimija/sessions")
            .join(&marker.id)
            .join("handoff.jsonl");
        let handoffs = if handoff_path.exists() {
            handoff::read_all(repo_root, &marker.id)?
        } else {
            Vec::new()
        };
        append_locked(
            &mut self.file,
            &self.path,
            &derive_record(marker, &handoffs, end, recorded_at),
        )?;
        self.recorded.insert(marker.id.clone());
        Ok(true)
    }
}

pub fn derive_record(
    marker: &SessionMarker,
    handoffs: &[handoff::HandoffRecord],
    end: HistoryEnd,
    recorded_at: u64,
) -> HistoryRecord {
    let notes: Vec<String> = handoffs
        .iter()
        .filter_map(|record| record.next_step.clone())
        .collect();
    let handoffs_total = notes.len();
    HistoryRecord {
        version: HISTORY_VERSION,
        id: marker.id.clone(),
        agent: marker.agent.clone(),
        intent: marker.intent.clone(),
        scope: marker.scope.clone(),
        branch: marker.branch.clone(),
        started_at: marker.started_at,
        last_seen_at: marker.last_seen_at,
        end,
        exit_status: marker.exit_status,
        handoffs: notes.into_iter().take(HANDOFF_CAP).collect(),
        handoffs_total,
        recorded_at,
    }
}

pub fn append_record(repo_root: &Path, record: &HistoryRecord) -> Result<()> {
    let (path, mut file) = open_history(repo_root)?;
    file.lock()
        .with_context(|| format!("failed to lock {}", path.display()))?;
    append_locked(&mut file, &path, record)
}

fn open_history(repo_root: &Path) -> Result<(std::path::PathBuf, fs::File)> {
    let path = repo_root.join(HISTORY_PATH);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .read(true)
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    Ok((path, file))
}

fn append_locked(file: &mut fs::File, path: &Path, record: &HistoryRecord) -> Result<()> {
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    file.write_all(&line)
        .with_context(|| format!("failed to append {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))?;
    info!(
        event = "toimija.footprint.journaled",
        session = %record.id,
        end = ?record.end
    );
    Ok(())
}

/// Read every valid history line. Malformed lines degrade the listing instead
/// of hiding all remaining records.
pub fn read_history(repo_root: &Path) -> Result<HistoryRead> {
    let path = repo_root.join(HISTORY_PATH);
    let mut file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HistoryRead {
                records: Vec::new(),
                malformed: 0,
            });
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to open {}", path.display()));
        }
    };
    file.lock_shared()
        .with_context(|| format!("failed to lock {}", path.display()))?;
    read_locked(&mut file, &path)
}

fn read_locked(file: &mut fs::File, path: &Path) -> Result<HistoryRead> {
    file.rewind()
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut records = Vec::new();
    let mut malformed = 0;
    for line in BufReader::new(file).lines() {
        let line = line.with_context(|| format!("failed to read {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(&line) {
            Ok(record) => records.push(record),
            Err(_) => malformed += 1,
        }
    }
    if malformed > 0 {
        warn!(
            event = "toimija.history.malformed_lines",
            path = %path.display(),
            malformed
        );
    }
    Ok(HistoryRead { records, malformed })
}

pub fn journal_session(
    repo_root: &Path,
    marker: &SessionMarker,
    end: HistoryEnd,
    recorded_at: u64,
) -> Result<bool> {
    Journal::open(repo_root)?.journal_session(repo_root, marker, end, recorded_at)
}

/// Durably remember a concluded session, then and only then remove its dir.
pub fn reap(
    repo_root: &Path,
    dir: &Path,
    marker: &SessionMarker,
    end: HistoryEnd,
    recorded_at: u64,
    journal: &mut Journal,
) -> Result<bool> {
    let journaled = journal.journal_session(repo_root, marker, end, recorded_at)?;
    fs::remove_dir_all(dir).with_context(|| format!("failed to remove {}", dir.display()))?;
    Ok(journaled)
}

pub fn render_history(read: &HistoryRead) -> String {
    if read.records.is_empty() {
        return if read.malformed == 0 {
            "No concluded sessions recorded in .toimija/history.jsonl.\n".to_string()
        } else {
            format!(
                "No valid history records ({} malformed line(s) skipped).\n",
                read.malformed
            )
        };
    }
    let mut records: Vec<&HistoryRecord> = read.records.iter().collect();
    records.sort_by(|a, b| {
        b.recorded_at
            .cmp(&a.recorded_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    let mut out = String::from("HISTORY\n");
    for record in records {
        let agent = record
            .agent
            .first()
            .map(String::as_str)
            .unwrap_or("unknown");
        out.push_str(&format!("  {agent}  {}\n", record.id));
        out.push_str(&format!("    end: {}\n", record.end.as_str()));
        out.push_str(&format!(
            "    branch: {}\n",
            record.branch.as_deref().unwrap_or("unknown")
        ));
        out.push_str(&format!(
            "    intent: {}\n",
            record.intent.as_deref().unwrap_or("unknown")
        ));
        if record.handoffs_total > 0 {
            out.push_str(&format!("    handoffs: {}\n", record.handoffs_total));
        }
    }
    if read.malformed > 0 {
        out.push_str(&format!(
            "WARNINGS\n  - {} malformed history line(s) skipped\n",
            read.malformed
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionStatus;

    fn marker(id: &str) -> SessionMarker {
        SessionMarker {
            id: id.to_string(),
            pid: 1,
            agent: vec!["codex".to_string()],
            worktree: "/repo".to_string(),
            branch: Some("main".to_string()),
            head: Some("abc".to_string()),
            intent: Some("ship".to_string()),
            scope: vec!["src".to_string()],
            started_at: 100,
            last_seen_at: Some(150),
            status: SessionStatus::Closed,
            closed_at: Some(200),
            exit_status: Some(0),
        }
    }

    #[test]
    fn registry_classifies_every_owned_artifact_shape() {
        for (path, class) in [
            (".toimija/repo.yaml", ArtifactClass::Config),
            (".toimija/current.md", ArtifactClass::LivePacket),
            (".toimija/current.json", ArtifactClass::LivePacket),
            (
                ".toimija/sessions/100-1/session.json",
                ArtifactClass::SessionDir,
            ),
            (".toimija/runs/kisko.jsonl", ArtifactClass::RunTranscript),
            (
                ".toimija/snapshots/lease.json",
                ArtifactClass::SnapshotLease,
            ),
            (".toimija/history.jsonl", ArtifactClass::HistoryJournal),
            (
                ".toimija/sessions/100-1/session.json.tmp-17582-9",
                ArtifactClass::TempFile,
            ),
        ] {
            assert_eq!(classify(Path::new(path)), Some(class), "{path}");
            assert!(class.contains(Path::new(path)));
            assert!(!class.owner().is_empty());
            assert!(!class.retention().is_empty());
        }
    }

    #[test]
    fn unknown_artifact_path_is_classified_as_none() {
        assert_eq!(classify(Path::new(".toimija/mystery.bin")), None);
        assert_eq!(classify(Path::new("src/session.json.tmp-1-2")), None);
        assert_eq!(classify(Path::new(".toimija/not-really.tmp-x-2")), None);
    }

    #[test]
    fn derive_record_caps_handoff_notes_and_keeps_the_note_total() {
        let handoffs: Vec<handoff::HandoffRecord> = (0..14)
            .map(|index| handoff::HandoffRecord {
                version: 1,
                session_id: "100-1".to_string(),
                written_at: index,
                agent: Vec::new(),
                agent_total: 0,
                intent: None,
                branch: None,
                entry_head: None,
                head: None,
                merge_base: None,
                staged: Vec::new(),
                staged_total: 0,
                unstaged: Vec::new(),
                unstaged_total: 0,
                untracked: Vec::new(),
                untracked_total: 0,
                conflicts: Vec::new(),
                conflicts_total: 0,
                branch_scope: Vec::new(),
                branch_scope_total: 0,
                suggested_validation: Vec::new(),
                suggested_validation_total: 0,
                coupling: Vec::new(),
                coupling_total: 0,
                risk: Vec::new(),
                risk_total: 0,
                consistency_token: String::new(),
                next_step: (![0, 7].contains(&index)).then(|| format!("note {index}")),
                sauma: None,
            })
            .collect();
        let record = derive_record(&marker("100-1"), &handoffs, HistoryEnd::Closed, 300);
        assert_eq!(record.handoffs.len(), 10);
        assert_eq!(record.handoffs_total, 12);
    }

    #[test]
    fn concurrent_journaling_records_a_session_once() {
        let temp = tempfile::tempdir().unwrap();
        let marker = marker("100-1");
        let barrier = std::sync::Barrier::new(8);

        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    journal_session(temp.path(), &marker, HistoryEnd::Closed, 300).unwrap();
                });
            }
        });

        assert_eq!(read_history(temp.path()).unwrap().records.len(), 1);
    }

    #[test]
    fn reap_appends_the_record_before_removing_the_directory() {
        let temp = tempfile::tempdir().unwrap();
        let marker = marker("100-1");
        let dir = temp.path().join(".toimija/sessions/100-1");
        fs::create_dir_all(&dir).unwrap();
        let mut journal = Journal::open(temp.path()).unwrap();

        assert!(
            reap(
                temp.path(),
                &dir,
                &marker,
                HistoryEnd::Closed,
                300,
                &mut journal
            )
            .unwrap()
        );
        assert!(!dir.exists());
        drop(journal);
        assert_eq!(read_history(temp.path()).unwrap().records.len(), 1);
    }

    #[test]
    fn failed_journal_open_keeps_the_session_directory() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".toimija/sessions/100-1");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(temp.path().join(HISTORY_PATH)).unwrap();

        assert!(Journal::open(temp.path()).is_err());
        assert!(dir.exists());
    }

    #[test]
    fn teardown_journaled_session_is_not_journaled_again_by_the_sweep() {
        let temp = tempfile::tempdir().unwrap();
        let marker = marker("100-1");
        let dir = temp.path().join(".toimija/sessions/100-1");
        fs::create_dir_all(&dir).unwrap();
        let mut journal = Journal::open(temp.path()).unwrap();

        assert!(
            journal
                .journal_session(temp.path(), &marker, HistoryEnd::Closed, 200)
                .unwrap()
        );
        assert!(
            !reap(
                temp.path(),
                &dir,
                &marker,
                HistoryEnd::Closed,
                300,
                &mut journal,
            )
            .unwrap()
        );
        drop(journal);
        assert_eq!(read_history(temp.path()).unwrap().records.len(), 1);
        assert!(!dir.exists());
    }

    #[test]
    fn unknown_history_fields_are_tolerated_and_malformed_lines_are_counted() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".toimija")).unwrap();
        let record = serde_json::to_string(&derive_record(
            &marker("100-1"),
            &[],
            HistoryEnd::Closed,
            300,
        ))
        .unwrap();
        let with_unknown = record.replacen("{", "{\"future_optional\":true,", 1);
        fs::write(
            temp.path().join(HISTORY_PATH),
            format!("{with_unknown}\n{{broken\n"),
        )
        .unwrap();

        let read = read_history(temp.path()).unwrap();
        assert_eq!(read.records.len(), 1);
        assert_eq!(read.malformed, 1);
    }
}
