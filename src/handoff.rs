//! Session handoff records (`toimija handoff`).
//!
//! Explicit, append-only, local JSONL. No exit hook, no daemon.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::command::CommandRunner;
use crate::context::{ContextPacket, build_context_packet};
use crate::git::ChangedFile;
use crate::sauma::{self, SaumaEnvelope, SaumaProvenance};
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
    #[serde(with = "crate::path_codec::vec")]
    pub staged: Vec<PathBuf>,
    pub staged_total: usize,
    #[serde(with = "crate::path_codec::vec")]
    pub unstaged: Vec<PathBuf>,
    pub unstaged_total: usize,
    #[serde(with = "crate::path_codec::vec")]
    pub untracked: Vec<PathBuf>,
    pub untracked_total: usize,
    #[serde(with = "crate::path_codec::vec")]
    pub conflicts: Vec<PathBuf>,
    pub conflicts_total: usize,
    #[serde(with = "crate::path_codec::vec")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sauma: Option<SaumaEnvelope>,
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
    let mut file = crate::owned::open(
        repo_root,
        &path,
        OpenOptions::new().create(true).append(true),
    )
    .context("failed to append the handoff record")?;
    serde_json::to_writer(&mut file, &record)?;
    file.write_all(b"\n")
        .with_context(|| format!("failed to write {}", path.display()))?;

    marker.last_seen_at = Some(now_secs());
    let json = serde_json::to_string_pretty(&marker)?;
    crate::owned::replace(repo_root, &marker_path, format!("{json}\n").as_bytes())
        .context("failed to write the session marker")?;

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

/// Count the handoff records for a session; `None` means no handoff file.
/// Unreadable ≠ missing (issue #161): a file that exists but cannot be opened
/// or read is warned about, and a malformed line is warned about and skipped
/// while the valid records are still counted — one bad line must not collapse
/// real handoff records to zero.
pub fn count(repo_root: &Path, id: &str) -> Option<usize> {
    let path = handoff_path(repo_root, id).ok()?;
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            warn!(
                event = "toimija.handoff.unreadable",
                path = %path.display(),
                error = %error
            );
            return None;
        }
    };
    let mut total = 0;
    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                warn!(
                    event = "toimija.handoff.unreadable",
                    path = %path.display(),
                    error = %error
                );
                return Some(total);
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<HandoffRecord>(&line) {
            Ok(_) => total += 1,
            Err(error) => warn!(
                event = "toimija.handoff.record_malformed",
                path = %path.display(),
                error = %error
            ),
        }
    }
    Some(total)
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
        let suggested = if let Some(gate) = &packet.validation_plan.gate {
            vec![gate.command.clone()]
        } else if packet.validation_plan.focused.is_empty() {
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
        let mut record = HandoffRecord {
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
            sauma: None,
        };
        record.attach_sauma().expect("handoff record serializes");
        record
    }

    fn attach_sauma(&mut self) -> serde_json::Result<()> {
        let mut provenance = vec![SaumaProvenance::Token {
            token: self.consistency_token.clone(),
        }];
        if let Some(oid) = self.head.clone() {
            provenance.push(SaumaProvenance::Git { oid });
        }
        let token = self.consistency_token.clone();
        let run = Some(self.session_id.clone());
        sauma::attach(self, "handoff-record.v1", provenance, &token, run)
    }
}

impl sauma::Carrier for HandoffRecord {
    fn sauma_mut(&mut self) -> &mut Option<SaumaEnvelope> {
        &mut self.sauma
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
    use crate::relevance::{
        CouplingHint, HintEvidence, RelevancePacket, RelevanceSource, RiskHint, RiskKind,
    };
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
            cycle_action: None,
            started_at: 100,
            last_seen_at: None,
            status,
            closed_at: None,
            exit_status: None,

            entry_upstream_oid: None,
            entry_open_prs: None,
            effects_observed: Vec::new(),
            effects_coverage: Default::default(),
            undelivered: Vec::new(),
        }
    }

    fn packet() -> ContextPacket {
        let paths: Vec<PathBuf> = (0..30)
            .map(|i| PathBuf::from(format!("src/{i}.rs")))
            .collect();
        ContextPacket {
            schema: crate::context::CONTEXT_SCHEMA.to_string(),
            artifact_id: None,
            repo: RepoInfo {
                name: "repo".to_string(),
                role: Some("role".to_string()),
                not_roles: Vec::new(),
                important_docs: Vec::new(),
                boundaries: Vec::new(),
            },
            repo_kind: RepoKind::Rust,
            secondary_languages: Vec::new(),
            refresh_token: None,
            git: GitState {
                repo_root: PathBuf::from("/repo"),
                git_dir: Some(PathBuf::from(".git")),
                branch: Some("feat/x".to_string()),
                head_oid: Some("head".to_string()),
                upstream: None,
                ahead: None,
                behind: None,
                ahead_behind_ref: None,
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
                        original_path: None,
                        similarity: None,
                    })
                    .collect(),
                unstaged_files: Vec::new(),
                untracked_files: paths.clone(),
                conflicted_files: paths.clone(),
                conflict_entries: Vec::new(),
                branch_files: Vec::new(),
                branch_diff_coverage: crate::coverage::Coverage::unrecorded(),
                recent_commits: Vec::new(),
            },
            changed_surface: paths,
            changed_surface_source: ChangedSurfaceSource::WorkingTreeAndIndex,
            read_first: Vec::<ReadFirstEntry>::new(),
            safe_to_ignore: Vec::new(),
            validation_plan: ValidationPlan {
                source: ValidationSource::Inferred,
                gate: None,
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
                    evidence: HintEvidence::default(),
                }],
                risk: vec![RiskHint {
                    path: PathBuf::from("src/lib.rs"),
                    kind: RiskKind::HighChurn,
                    reason: "high churn".to_string(),
                    action: "keep the change small".to_string(),
                    source: RelevanceSource::GitLog,
                    evidence: HintEvidence::default(),
                }],
                warnings: Vec::new(),
                history_coverage: crate::coverage::Coverage::unrecorded(),
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
            session: None,
            sauma: None,
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
        assert_eq!(record.sauma.as_ref().unwrap().schema, "handoff-record.v1");
        assert_eq!(
            record.sauma.as_ref().unwrap().producer.run.as_deref(),
            Some("100-1")
        );
        assert_eq!(
            record.sauma.as_ref().unwrap().anchored_at,
            record.consistency_token
        );
    }

    #[test]
    fn handoff_sauma_digest_round_trips_without_self_reference() {
        let record =
            HandoffRecord::from_marker_packet(&marker(SessionStatus::Active), &packet(), None);
        let digest = record.sauma.as_ref().unwrap().digest.clone();
        let value = serde_json::to_value(&record).unwrap();

        assert_eq!(crate::sauma::artifact_digest_value(&value), digest);
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
    fn read_all_tolerates_old_records_without_sauma() {
        let temp = tempfile::tempdir().unwrap();
        let id = "100-1";
        let dir = temp.path().join(SESSIONS_SUBDIR).join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(FILE_NAME),
            r#"{"version":1,"session_id":"100-1","written_at":1,"agent":[],"agent_total":0,"intent":null,"branch":null,"entry_head":null,"head":null,"merge_base":null,"staged":[],"staged_total":0,"unstaged":[],"unstaged_total":0,"untracked":[],"untracked_total":0,"conflicts":[],"conflicts_total":0,"branch_scope":[],"branch_scope_total":0,"suggested_validation":[],"suggested_validation_total":0,"coupling":[],"coupling_total":0,"risk":[],"risk_total":0,"consistency_token":"t","next_step":null}"#,
        )
        .unwrap();
        let records = read_all(temp.path(), id).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].sauma.is_none());
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
    fn count_still_counts_valid_records_around_a_malformed_line() {
        let temp = tempfile::tempdir().unwrap();
        let id = "100-1";
        let dir = temp.path().join(SESSIONS_SUBDIR).join(id);
        fs::create_dir_all(&dir).unwrap();
        let record =
            HandoffRecord::from_marker_packet(&marker(SessionStatus::Active), &packet(), None);
        let valid = serde_json::to_string(&record).unwrap();
        fs::write(dir.join(FILE_NAME), format!("{valid}\nnot json\n{valid}\n")).unwrap();
        // Issue #161: one bad line must not collapse real records to zero.
        assert_eq!(count(temp.path(), id), Some(2));
    }

    #[test]
    fn count_is_none_when_the_handoff_file_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(SESSIONS_SUBDIR).join("100-1")).unwrap();
        assert_eq!(count(temp.path(), "100-1"), None);
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
