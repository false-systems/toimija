//! Ownership and durable history for files under `.toimija/`.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::atomic_write::{self, TEMP_MARKER};
use crate::command::CommandRunner;
use crate::handoff;
use crate::session::SessionMarker;
use crate::sessions::{self, Liveness};

pub const HISTORY_PATH: &str = ".toimija/history.jsonl";
pub const HISTORY_LOCK_PATH: &str = ".toimija/history.lock";
const HISTORY_VERSION: u32 = 1;
const HANDOFF_CAP: usize = 10;
pub const DEFAULT_RUNS_RETENTION_SECS: u64 = 14 * 24 * 60 * 60;
pub const ORPHAN_TEMP_RETENTION_SECS: u64 = 60 * 60;
pub const DEFAULT_HISTORY_MAX_RECORDS: usize = 500;

#[derive(Debug, Clone, Copy)]
pub struct SweepOptions {
    pub dry_run: bool,
    pub sessions_retention_secs: u64,
    pub runs_retention_secs: u64,
    pub history_max_records: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorPosture {
    Explicit,
    Opportunistic,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SweepReport {
    pub sessions_removed: Vec<String>,
    pub runs_removed: Vec<String>,
    pub temps_removed: Vec<String>,
    pub journaled: usize,
    pub kept: usize,
    pub dry_run: bool,
}

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
    if path == Path::new(HISTORY_PATH) || path == Path::new(HISTORY_LOCK_PATH) {
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
    _lock: fs::File,
    path: PathBuf,
    file: fs::File,
    recorded: BTreeSet<String>,
}

impl Journal {
    pub fn open(repo_root: &Path) -> Result<Self> {
        let lock = lock_history(repo_root)?;
        let (path, mut file) = open_history_file(repo_root)?;
        file.lock()
            .with_context(|| format!("failed to lock {}", path.display()))?;
        let recorded = read_locked(&mut file, &path)?
            .records
            .into_iter()
            .map(|record| record.id)
            .collect();
        Ok(Self {
            _lock: lock,
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
    let _lock = lock_history(repo_root)?;
    let (path, mut file) = open_history_file(repo_root)?;
    file.lock()
        .with_context(|| format!("failed to lock {}", path.display()))?;
    append_locked(&mut file, &path, record)
}

fn lock_history(repo_root: &Path) -> Result<fs::File> {
    let path = repo_root.join(HISTORY_LOCK_PATH);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.lock()
        .with_context(|| format!("failed to lock {}", path.display()))?;
    Ok(file)
}

fn open_history_file(repo_root: &Path) -> Result<(std::path::PathBuf, fs::File)> {
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

/// Reclaim every bounded Toimija artifact in one pass. Explicit cleanup
/// propagates failures; opportunistic entry cleanup emits one warning and
/// stops, so housekeeping never blocks the user's work.
pub fn sweep(
    runner: &dyn CommandRunner,
    repo_root: &Path,
    options: SweepOptions,
    posture: ErrorPosture,
) -> Result<Option<SweepReport>> {
    match sweep_inner(runner, repo_root, options) {
        Ok(report) => Ok(Some(report)),
        Err(error) if posture == ErrorPosture::Opportunistic => {
            warn!(event = "toimija.footprint.sweep_failed", error = %error);
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn sweep_inner(
    runner: &dyn CommandRunner,
    repo_root: &Path,
    options: SweepOptions,
) -> Result<SweepReport> {
    let now = now_secs();
    let plan = sessions::reap_plan(
        runner,
        repo_root,
        sessions::DEFAULT_STALE_AFTER_SECS,
        options.sessions_retention_secs,
    )?;
    let mut kept = plan.kept;
    let mut run_candidates = Vec::new();
    for path in direct_children(&repo_root.join(".toimija/runs"))? {
        if is_atomic_temp(&path) {
            continue;
        }
        if older_than(&path, now, options.runs_retention_secs)? {
            run_candidates.push(path);
        } else {
            kept += 1;
        }
    }
    let mut temp_candidates = Vec::new();
    for path in descendants(&repo_root.join(".toimija"))? {
        if !is_atomic_temp(&path)
            || !fs::symlink_metadata(&path)
                .with_context(|| format!("failed to inspect {}", path.display()))?
                .file_type()
                .is_file()
        {
            continue;
        }
        if older_than(&path, now, ORPHAN_TEMP_RETENTION_SECS)? {
            temp_candidates.push(path);
        } else {
            kept += 1;
        }
    }
    run_candidates.sort();
    temp_candidates.sort();

    let mut report = SweepReport {
        sessions_removed: plan
            .candidates
            .iter()
            .map(|candidate| candidate.marker.id.clone())
            .collect(),
        runs_removed: run_candidates
            .iter()
            .map(|path| relative(repo_root, path))
            .collect(),
        temps_removed: temp_candidates
            .iter()
            .map(|path| relative(repo_root, path))
            .collect(),
        journaled: 0,
        kept,
        dry_run: options.dry_run,
    };

    if options.dry_run {
        let recorded: BTreeSet<String> = read_history(repo_root)?
            .records
            .into_iter()
            .map(|record| record.id)
            .collect();
        report.journaled = plan
            .candidates
            .iter()
            .filter(|candidate| !recorded.contains(&candidate.marker.id))
            .count();
    } else {
        for path in &temp_candidates {
            fs::remove_file(path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
        for path in &run_candidates {
            remove_artifact(path)?;
        }
        let mut journal = None;
        for candidate in &plan.candidates {
            let end = match candidate.liveness {
                Liveness::Closed => HistoryEnd::Closed,
                Liveness::Stale => HistoryEnd::ReapedStale,
                Liveness::Active => unreachable!("active sessions are never reap candidates"),
            };
            let journal = match &mut journal {
                Some(journal) => journal,
                None => journal.insert(Journal::open(repo_root)?),
            };
            if reap(
                repo_root,
                &candidate.dir,
                &candidate.marker,
                end,
                now,
                journal,
            )? {
                report.journaled += 1;
            }
        }
        drop(journal);
        compact_history(repo_root, options.history_max_records)?;
    }

    info!(
        event = "toimija.footprint.swept",
        sessions_removed = report.sessions_removed.len(),
        runs_removed = report.runs_removed.len(),
        temps_removed = report.temps_removed.len(),
        journaled = report.journaled,
        kept = report.kept,
        dry_run = report.dry_run
    );
    Ok(report)
}

fn direct_children(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", dir.display()));
        }
    };
    entries
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .with_context(|| format!("failed to read {}", dir.display()))
        })
        .collect()
}

fn descendants(root: &Path) -> Result<Vec<PathBuf>> {
    let mut pending = direct_children(root)?;
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        if fs::symlink_metadata(&path)
            .with_context(|| format!("failed to inspect {}", path.display()))?
            .file_type()
            .is_dir()
        {
            pending.extend(direct_children(&path)?);
        } else {
            files.push(path);
        }
    }
    Ok(files)
}

fn older_than(path: &Path, now: u64, retention_secs: u64) -> Result<bool> {
    let modified = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .modified()
        .with_context(|| format!("failed to read modification time for {}", path.display()))?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(now.saturating_sub(modified) > retention_secs)
}

fn remove_artifact(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .with_context(|| format!("failed to remove {}", path.display()))
}

fn compact_history(repo_root: &Path, max_records: usize) -> Result<()> {
    let path = repo_root.join(HISTORY_PATH);
    if !path.exists() {
        return Ok(());
    }
    let _lock = lock_history(repo_root)?;
    let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    file.lock()
        .with_context(|| format!("failed to lock {}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let Some(compacted) = select_compaction(&bytes, max_records) else {
        return Ok(());
    };
    atomic_write::replace(&path, &compacted)
        .with_context(|| format!("failed to compact {}", path.display()))
}

fn select_compaction(bytes: &[u8], max_records: usize) -> Option<Vec<u8>> {
    let lines: Vec<&[u8]> = bytes
        .split_inclusive(|byte| *byte == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .collect();
    if lines.len() <= max_records {
        return None;
    }
    Some(
        lines[lines.len() - max_records..]
            .iter()
            .flat_map(|line| line.iter().copied())
            .collect(),
    )
}

fn relative(repo_root: &Path, path: &Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

pub fn render_sweep(report: &SweepReport) -> String {
    let verb = if report.dry_run {
        "would remove"
    } else {
        "removed"
    };
    let journal = if report.dry_run {
        "would journal"
    } else {
        "journaled"
    };
    let mut out = format!(
        "{verb} {} sessions, {} transcripts, {} temp files; {journal} {} records; kept {}\n",
        report.sessions_removed.len(),
        report.runs_removed.len(),
        report.temps_removed.len(),
        report.journaled,
        report.kept
    );
    for (kind, paths) in [
        ("session", &report.sessions_removed),
        ("transcript", &report.runs_removed),
        ("temp", &report.temps_removed),
    ] {
        for path in paths {
            out.push_str(&format!("  {kind}: {path}\n"));
        }
    }
    out
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
    use crate::command::{CommandOutput, CommandRunner};
    use crate::session::SessionStatus;

    struct DeadRunner;

    impl CommandRunner for DeadRunner {
        fn run(
            &self,
            _cwd: &Path,
            _program: &str,
            _args: &[&str],
        ) -> std::io::Result<CommandOutput> {
            Ok(CommandOutput {
                status: 1,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
    }

    fn set_age(path: &Path, age_secs: u64) {
        let modified = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }

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
            (".toimija/history.lock", ArtifactClass::HistoryJournal),
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
    fn a_reaped_lid_death_records_its_frozen_heartbeat() {
        let temp = tempfile::tempdir().unwrap();
        let mut marker = marker("100-1");
        marker.pid = 2_000_000_000;
        marker.status = SessionStatus::Active;
        marker.last_seen_at = Some(150);
        marker.closed_at = None;
        marker.exit_status = None;
        let dir = temp.path().join(".toimija/sessions/100-1");
        fs::create_dir_all(&dir).unwrap();
        let mut journal = Journal::open(temp.path()).unwrap();

        reap(
            temp.path(),
            &dir,
            &marker,
            HistoryEnd::ReapedStale,
            10_000,
            &mut journal,
        )
        .unwrap();
        drop(journal);

        let record = read_history(temp.path()).unwrap().records.pop().unwrap();
        assert_eq!(record.end, HistoryEnd::ReapedStale);
        assert_eq!(record.started_at, 100);
        assert_eq!(record.last_seen_at, Some(150));
        assert!(!dir.exists());
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

    #[test]
    fn sweep_dry_run_and_cleanup_share_exact_candidates() {
        let temp = tempfile::tempdir().unwrap();
        let toimija = temp.path().join(".toimija");
        let runs = toimija.join("runs");
        fs::create_dir_all(&runs).unwrap();
        let old_run = runs.join("old.jsonl");
        let fresh_run = runs.join("fresh.jsonl");
        let old_temp = toimija.join("current.json.tmp-123-4");
        let fresh_temp = toimija.join("current.md.tmp-123-5");
        let false_temp = toimija.join("current.json.tmp-bad-6");
        for path in [&old_run, &fresh_run, &old_temp, &fresh_temp, &false_temp] {
            fs::write(path, b"x").unwrap();
        }
        set_age(&old_run, 20);
        set_age(&old_temp, ORPHAN_TEMP_RETENTION_SECS + 1);
        set_age(&false_temp, ORPHAN_TEMP_RETENTION_SECS + 1);
        let options = SweepOptions {
            dry_run: true,
            sessions_retention_secs: sessions::DEFAULT_RETENTION_SECS,
            runs_retention_secs: 10,
            history_max_records: DEFAULT_HISTORY_MAX_RECORDS,
        };

        let dry = sweep(&DeadRunner, temp.path(), options, ErrorPosture::Explicit)
            .unwrap()
            .unwrap();
        assert_eq!(dry.runs_removed, vec![".toimija/runs/old.jsonl"]);
        assert_eq!(dry.temps_removed, vec![".toimija/current.json.tmp-123-4"]);
        assert!(old_run.exists());
        assert!(old_temp.exists());

        let cleaned = sweep(
            &DeadRunner,
            temp.path(),
            SweepOptions {
                dry_run: false,
                ..options
            },
            ErrorPosture::Explicit,
        )
        .unwrap()
        .unwrap();
        assert_eq!(cleaned.runs_removed, dry.runs_removed);
        assert_eq!(cleaned.temps_removed, dry.temps_removed);
        assert!(!old_run.exists());
        assert!(!old_temp.exists());
        assert!(fresh_run.exists());
        assert!(fresh_temp.exists());
        assert!(false_temp.exists());
    }

    #[test]
    fn sweep_compacts_history_to_the_newest_whole_records() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..4 {
            append_record(
                temp.path(),
                &derive_record(
                    &marker(&format!("100-{index}")),
                    &[],
                    HistoryEnd::Closed,
                    index,
                ),
            )
            .unwrap();
        }

        sweep(
            &DeadRunner,
            temp.path(),
            SweepOptions {
                dry_run: false,
                sessions_retention_secs: sessions::DEFAULT_RETENTION_SECS,
                runs_retention_secs: DEFAULT_RUNS_RETENTION_SECS,
                history_max_records: 2,
            },
            ErrorPosture::Explicit,
        )
        .unwrap();

        let ids: Vec<String> = read_history(temp.path())
            .unwrap()
            .records
            .into_iter()
            .map(|record| record.id)
            .collect();
        assert_eq!(ids, ["100-2", "100-3"]);
    }

    #[test]
    fn compaction_selects_the_newest_whole_lines() {
        assert_eq!(
            select_compaction(b"one\ntwo\nthree\n", 2),
            Some(b"two\nthree\n".to_vec())
        );
        assert_eq!(select_compaction(b"one\ntwo\n", 2), None);
    }
}
