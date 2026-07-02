//! Session handoff records (`toimija handoff`).
//!
//! Explicit, append-only, local JSONL. No exit hook, no daemon.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::command::CommandRunner;
use crate::context::{ContextPacket, build_context_packet};
use crate::git::ChangedFile;
use crate::session::{SESSIONS_SUBDIR, SessionMarker, SessionStatus, is_valid_session_id};

const VERSION: u32 = 1;
const CAP: usize = 25;
const FILE_NAME: &str = "handoff.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffRecord {
    pub version: u32,
    pub session_id: String,
    pub written_at: u64,
    pub agent: Vec<String>,
    pub agent_total: usize,
    pub intent: Option<String>,
    pub branch: Option<String>,
    pub entry_head: Option<String>,
    pub head: Option<String>,
    pub merge_base: Option<String>,
    pub staged: Vec<PathBuf>,
    pub staged_total: usize,
    pub unstaged: Vec<PathBuf>,
    pub unstaged_total: usize,
    pub untracked: Vec<PathBuf>,
    pub untracked_total: usize,
    pub conflicts: Vec<PathBuf>,
    pub conflicts_total: usize,
    pub branch_scope: Vec<PathBuf>,
    pub branch_scope_total: usize,
    pub suggested_validation: Vec<String>,
    pub suggested_validation_total: usize,
    pub coupling: Vec<String>,
    pub coupling_total: usize,
    pub risk: Vec<String>,
    pub risk_total: usize,
    pub consistency_token: String,
    pub next_step: Option<String>,
}

pub fn write(
    runner: &impl CommandRunner,
    repo_root: &Path,
    id: &str,
    note: Option<String>,
) -> Result<HandoffRecord> {
    let marker_path = marker_path(repo_root, id)?;
    let mut marker = read_marker(&marker_path, id)?;
    anyhow::ensure!(
        marker.status == SessionStatus::Active,
        "session {id} is closed; handoff can only be written by a live session"
    );
    let packet =
        build_context_packet(runner, repo_root, true).context("failed to build handoff packet")?;
    let record = HandoffRecord::from_marker_packet(&marker, &packet, note);
    let path = handoff_path(repo_root, id)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    serde_json::to_writer(&mut file, &record)?;
    file.write_all(b"\n")
        .with_context(|| format!("failed to write {}", path.display()))?;

    marker.last_seen_at = Some(now_secs());
    let json = serde_json::to_string_pretty(&marker)?;
    fs::write(&marker_path, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", marker_path.display()))?;

    let records_total = count_records(&path).unwrap_or(0);
    info!(
        event = "toimija.handoff.written",
        session = %id,
        records_total
    );
    Ok(record)
}

pub fn read_all(repo_root: &Path, id: &str) -> Result<Vec<HandoffRecord>> {
    let path = handoff_path(repo_root, id)?;
    let file =
        fs::File::open(&path).with_context(|| format!("no handoff recorded for session {id}"))?;
    let mut records = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.with_context(|| format!("failed to read {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        records.push(
            serde_json::from_str(&line)
                .with_context(|| format!("failed to parse {}", path.display()))?,
        );
    }
    info!(
        event = "toimija.handoff.shown",
        session = %id,
        records = records.len()
    );
    Ok(records)
}

pub fn count(repo_root: &Path, id: &str) -> Option<usize> {
    count_records(&handoff_path(repo_root, id).ok()?).ok()
}

impl HandoffRecord {
    pub(crate) fn from_marker_packet(
        marker: &SessionMarker,
        packet: &ContextPacket,
        next_step: Option<String>,
    ) -> HandoffRecord {
        let (agent, agent_total) = cap(marker.agent.clone());
        let staged_paths: Vec<PathBuf> = packet.git.staged_files.iter().map(changed_path).collect();
        let unstaged_paths: Vec<PathBuf> =
            packet.git.unstaged_files.iter().map(changed_path).collect();
        let suggested = if packet.validation_plan.focused.is_empty() {
            packet.validation_plan.fast.clone()
        } else {
            packet.validation_plan.focused.clone()
        };
        let coupling: Vec<String> = packet
            .relevance
            .coupling
            .iter()
            .map(|hint| {
                format!(
                    "{} changed, but {} was not — {}",
                    hint.changed.display(),
                    hint.missing.display(),
                    hint.reason
                )
            })
            .collect();
        let risk: Vec<String> = packet
            .relevance
            .risk
            .iter()
            .map(|hint| format!("{} — {}; {}", hint.path.display(), hint.reason, hint.action))
            .collect();
        let (staged, staged_total) = cap(staged_paths);
        let (unstaged, unstaged_total) = cap(unstaged_paths);
        let (untracked, untracked_total) = cap(packet.git.untracked_files.clone());
        let (conflicts, conflicts_total) = cap(packet.git.conflicted_files.clone());
        let (branch_scope, branch_scope_total) = cap(packet.changed_surface.clone());
        let (suggested_validation, suggested_validation_total) = cap(suggested);
        let (coupling, coupling_total) = cap(coupling);
        let (risk, risk_total) = cap(risk);
        HandoffRecord {
            version: VERSION,
            session_id: marker.id.clone(),
            written_at: now_secs(),
            agent,
            agent_total,
            intent: marker.intent.clone(),
            branch: packet.git.branch.clone(),
            entry_head: marker.head.clone(),
            head: packet.git.head_oid.clone(),
            merge_base: packet.git.merge_base.clone(),
            staged,
            staged_total,
            unstaged,
            unstaged_total,
            untracked,
            untracked_total,
            conflicts,
            conflicts_total,
            branch_scope,
            branch_scope_total,
            suggested_validation,
            suggested_validation_total,
            coupling,
            coupling_total,
            risk,
            risk_total,
            consistency_token: packet.consistency.token.clone(),
            next_step,
        }
    }
}

fn marker_path(repo_root: &Path, id: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        is_valid_session_id(id),
        "invalid session id `{id}` — expected the `<unix-secs>-<pid>` shape from TOIMIJA_SESSION"
    );
    Ok(repo_root
        .join(SESSIONS_SUBDIR)
        .join(id)
        .join("session.json"))
}

fn handoff_path(repo_root: &Path, id: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        is_valid_session_id(id),
        "invalid session id `{id}` — expected the `<unix-secs>-<pid>` shape from TOIMIJA_SESSION"
    );
    Ok(repo_root.join(SESSIONS_SUBDIR).join(id).join(FILE_NAME))
}

fn read_marker(path: &Path, id: &str) -> Result<SessionMarker> {
    let raw = fs::read_to_string(path).with_context(|| {
        format!(
            "no session marker at {} — is the session still open?",
            path.display()
        )
    })?;
    let marker: SessionMarker = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    anyhow::ensure!(
        marker.id == id,
        "session marker id mismatch: expected {id}, found {}",
        marker.id
    );
    Ok(marker)
}

fn count_records(path: &Path) -> Result<usize> {
    let file = fs::File::open(path)?;
    Ok(BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .count())
}

fn changed_path(file: &ChangedFile) -> PathBuf {
    file.path.clone()
}

fn cap<T>(items: Vec<T>) -> (Vec<T>, usize) {
    let total = items.len();
    (items.into_iter().take(CAP).collect(), total)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consistency::{ConflictState, Consistency, GitOperationState, HeadState};
    use crate::context::{ChangedSurfaceSource, ContextPacket, ReadFirstEntry};
    use crate::git::{ChangeStatus, GitState};
    use crate::relevance::{CouplingHint, RelevancePacket, RelevanceSource, RiskHint, RiskKind};
    use crate::repo::{RepoInfo, RepoKind};
    use crate::validate::{ValidationPlan, ValidationSource};

    fn marker(status: SessionStatus) -> SessionMarker {
        SessionMarker {
            id: "100-1".to_string(),
            pid: 1,
            agent: (0..30).map(|i| format!("arg{i}")).collect(),
            worktree: "/repo".to_string(),
            branch: Some("entry".to_string()),
            head: Some("entry-head".to_string()),
            intent: Some("finish handoff".to_string()),
            scope: Vec::new(),
            started_at: 100,
            last_seen_at: None,
            status,
            closed_at: None,
            exit_status: None,
        }
    }

    fn packet() -> ContextPacket {
        let paths: Vec<PathBuf> = (0..30)
            .map(|i| PathBuf::from(format!("src/{i}.rs")))
            .collect();
        ContextPacket {
            repo: RepoInfo {
                name: "repo".to_string(),
                role: Some("role".to_string()),
                not_roles: Vec::new(),
                important_docs: Vec::new(),
                boundaries: Vec::new(),
            },
            repo_kind: RepoKind::Rust,
            git: GitState {
                repo_root: PathBuf::from("/repo"),
                git_dir: Some(PathBuf::from(".git")),
                branch: Some("feat/x".to_string()),
                head_oid: Some("head".to_string()),
                upstream: None,
                base: Some("origin/main".to_string()),
                merge_base: Some("base".to_string()),
                detached: false,
                dirty: true,
                staged_files: paths
                    .iter()
                    .cloned()
                    .map(|path| ChangedFile {
                        path,
                        status: ChangeStatus::Modified,
                        index_entry: None,
                    })
                    .collect(),
                unstaged_files: Vec::new(),
                untracked_files: paths.clone(),
                conflicted_files: paths.clone(),
                branch_files: Vec::new(),
                recent_commits: Vec::new(),
            },
            changed_surface: paths,
            changed_surface_source: ChangedSurfaceSource::WorkingTreeAndIndex,
            read_first: Vec::<ReadFirstEntry>::new(),
            safe_to_ignore: Vec::new(),
            validation_plan: ValidationPlan {
                source: ValidationSource::Inferred,
                focused: (0..30).map(|i| format!("cargo test {i}")).collect(),
                fast: vec!["cargo test".to_string()],
                strict: Vec::new(),
                full: Vec::new(),
                privileged: Vec::new(),
                notes: Vec::new(),
            },
            warnings: Vec::new(),
            for_diff: true,
            default_budget: None,
            relevance: RelevancePacket {
                scope: None,
                read_order: Vec::new(),
                coupling: vec![CouplingHint {
                    changed: PathBuf::from("src/lib.rs"),
                    missing: PathBuf::from("tests/lib.rs"),
                    reason: "changed together".to_string(),
                    source: RelevanceSource::GitLog,
                }],
                risk: vec![RiskHint {
                    path: PathBuf::from("src/lib.rs"),
                    kind: RiskKind::HighChurn,
                    reason: "high churn".to_string(),
                    action: "keep the change small".to_string(),
                    source: RelevanceSource::GitLog,
                }],
                warnings: Vec::new(),
            },
            consistency: Consistency {
                algorithm: "test".to_string(),
                head: HeadState::Normal {
                    branch: "feat/x".to_string(),
                    oid: "head".to_string(),
                },
                index_fingerprint: "i".to_string(),
                worktree_fingerprint: "w".to_string(),
                untracked_fingerprint: "u".to_string(),
                git_operation: GitOperationState::None,
                conflict_state: ConflictState {
                    has_conflicts: false,
                    files: Vec::new(),
                },
                token: "token".to_string(),
            },
        }
    }

    fn write_marker(repo: &Path, marker: &SessionMarker) {
        let dir = repo.join(SESSIONS_SUBDIR).join(&marker.id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("session.json"),
            format!("{}\n", serde_json::to_string_pretty(marker).unwrap()),
        )
        .unwrap();
    }

    #[test]
    fn derivation_caps_lists_with_honest_totals() {
        let record =
            HandoffRecord::from_marker_packet(&marker(SessionStatus::Active), &packet(), None);
        assert_eq!(record.version, 1);
        assert_eq!(record.agent.len(), 25);
        assert_eq!(record.agent_total, 30);
        assert_eq!(record.staged.len(), 25);
        assert_eq!(record.staged_total, 30);
        assert_eq!(record.branch_scope.len(), 25);
        assert_eq!(record.branch_scope_total, 30);
        assert_eq!(record.suggested_validation.len(), 25);
        assert_eq!(record.suggested_validation_total, 30);
        assert_eq!(
            record.coupling[0],
            "src/lib.rs changed, but tests/lib.rs was not — changed together"
        );
        assert_eq!(
            record.risk[0],
            "src/lib.rs — high churn; keep the change small"
        );
    }

    #[test]
    fn read_all_tolerates_unknown_optional_json_fields() {
        let temp = tempfile::tempdir().unwrap();
        let id = "100-1";
        let dir = temp.path().join(SESSIONS_SUBDIR).join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(FILE_NAME),
            r#"{"version":1,"session_id":"100-1","written_at":1,"agent":[],"agent_total":0,"intent":null,"branch":null,"entry_head":null,"head":null,"merge_base":null,"staged":[],"staged_total":0,"unstaged":[],"unstaged_total":0,"untracked":[],"untracked_total":0,"conflicts":[],"conflicts_total":0,"branch_scope":[],"branch_scope_total":0,"suggested_validation":[],"suggested_validation_total":0,"coupling":[],"coupling_total":0,"risk":[],"risk_total":0,"consistency_token":"t","next_step":null,"future_optional":"ok"}"#,
        )
        .unwrap();
        assert_eq!(read_all(temp.path(), id).unwrap().len(), 1);
    }

    #[test]
    fn read_all_refuses_traversal_ids() {
        let error = read_all(Path::new("."), "../../evil").unwrap_err();
        assert!(error.to_string().contains("invalid session id"));
    }

    #[test]
    fn closed_session_refusal_uses_marker_status() {
        let temp = tempfile::tempdir().unwrap();
        let marker = marker(SessionStatus::Closed);
        write_marker(temp.path(), &marker);
        let marker_path = marker_path(temp.path(), &marker.id).unwrap();
        let error = read_marker(&marker_path, &marker.id)
            .and_then(|marker| {
                anyhow::ensure!(
                    marker.status == SessionStatus::Active,
                    "session {} is closed; handoff can only be written by a live session",
                    marker.id
                );
                Ok(())
            })
            .unwrap_err();
        assert!(error.to_string().contains("closed"));
    }

    #[test]
    fn append_read_preserves_record_order() {
        let temp = tempfile::tempdir().unwrap();
        let id = "100-1";
        let dir = temp.path().join(SESSIONS_SUBDIR).join(id);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        let mut first = HandoffRecord::from_marker_packet(
            &marker(SessionStatus::Active),
            &packet(),
            Some("first".to_string()),
        );
        first.written_at = 1;
        let mut second = first.clone();
        second.next_step = Some("second".to_string());
        second.written_at = 2;
        fs::write(
            &path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&first).unwrap(),
                serde_json::to_string(&second).unwrap()
            ),
        )
        .unwrap();
        let records = read_all(temp.path(), id).unwrap();
        assert_eq!(records[0].next_step.as_deref(), Some("first"));
        assert_eq!(records[1].next_step.as_deref(), Some("second"));
    }
}
