use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::command::{CommandRunner, ProcessCommandRunner};
use crate::consistency::{Consistency, GitOperationState, Verdict};
use crate::context::ContextPacket;

pub mod store;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

id_type!(SpecId);
id_type!(WorkId);
id_type!(GoalId);
id_type!(ObligationId);
id_type!(GateId);
id_type!(TreeOid);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Spec {
    pub id: SpecId,
    pub title: String,
    pub objective: String,
    pub goals: Vec<Goal>,
    pub non_goals: Vec<String>,
    pub invariants: Vec<String>,
    pub verification: Vec<GateRequirement>,
    pub completion: CompletionLevel,
    pub status: SpecStatus,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Goal {
    pub id: GoalId,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateRequirement {
    pub id: GateId,
    pub command: GateCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateCommand {
    pub shell: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpecStatus {
    Draft,
    Ratified,
    Implemented,
    Superseded,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CompletionLevel {
    Changed,
    Validated,
}

impl fmt::Display for CompletionLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Changed => "changed",
            Self::Validated => "validated",
        })
    }
}

impl FromStr for CompletionLevel {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "changed" => Ok(Self::Changed),
            "validated" => Ok(Self::Validated),
            _ => bail!("completion must be `changed` or `validated`"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Work {
    pub id: WorkId,
    pub spec_id: SpecId,
    pub objective: String,
    pub scope: Scope,
    pub obligations: Vec<Obligation>,
    pub completion: CompletionLevel,
    pub origin: RepositoryPoint,
    pub status: WorkStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
    Open,
    Active,
    Verified,
    Closed,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryPoint {
    pub token: Consistency,
    pub tree_oid: TreeOid,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Scope {
    pub write: Vec<PathRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathRule {
    #[serde(with = "crate::path_codec::one")]
    pub path: PathBuf,
}

impl PathRule {
    pub fn new(path: PathBuf) -> Result<Self> {
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::RootDir))
        {
            bail!("scope paths must be non-empty repository-relative paths without `..`");
        }
        Ok(Self { path })
    }

    fn matches(&self, path: &Path) -> bool {
        self.path == Path::new(".") || path.starts_with(&self.path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Obligation {
    pub id: ObligationId,
    pub description: String,
    pub kind: ObligationKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObligationKind {
    Manual,
    Gate { gate_id: GateId },
    Scope,
    RepositoryChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateReceipt {
    pub work_id: WorkId,
    pub gate_id: GateId,
    pub tree_oid: TreeOid,
    pub command: GateCommand,
    pub exit_status: i32,
    pub output_digest: String,
    pub passed_at: u64,
}

impl GateReceipt {
    pub fn digest(stdout: &[u8], stderr: &[u8]) -> String {
        let mut digest = Sha256::new();
        digest.update(stdout);
        digest.update([0]);
        digest.update(stderr);
        format!("sha256:{:x}", digest.finalize())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManualClaim {
    pub work_id: WorkId,
    pub obligation_id: ObligationId,
    pub claimed_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompletionWitness {
    pub work_id: WorkId,
    pub repository: RepositoryPoint,
    pub closed_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceSet {
    pub gate_receipts: Vec<GateReceipt>,
    pub manual_claims: Vec<ManualClaim>,
    pub completion_witness: Option<CompletionWitness>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifiedRepository {
    pub point: RepositoryPoint,
    #[serde(with = "crate::path_codec::vec")]
    pub changed_paths: Vec<PathBuf>,
    pub dirty: bool,
}

/// Build current repository truth through Toimija, prove that it did not move
/// while being observed, then materialize the exact worktree tree.
pub fn verified_repository(cwd: &Path) -> Result<VerifiedRepository> {
    verified_repository_with(&ProcessCommandRunner, cwd)
}

pub fn verified_repository_with(
    runner: &impl CommandRunner,
    cwd: &Path,
) -> Result<VerifiedRepository> {
    let packet = crate::context::build_context_packet(runner, cwd, false)
        .context("failed to compile current Toimija state")?;
    verify_packet_is_current(runner, cwd, &packet)?;
    if packet.consistency.git_operation != GitOperationState::None {
        bail!(
            "current Toimija state is unsafe: {:?}",
            packet.consistency.git_operation
        );
    }
    if packet.consistency.conflict_state.has_conflicts {
        bail!("current Toimija state has unresolved conflicts");
    }
    reject_mixed_index_and_worktree(&packet)?;
    let tree_oid = TreeOid(
        crate::snapshot::materialize(runner, &packet.git.repo_root)
            .context("failed to materialize current repository tree")?,
    );
    verify_packet_is_current(runner, cwd, &packet)?;
    Ok(VerifiedRepository {
        dirty: packet
            .git
            .staged_files
            .iter()
            .chain(&packet.git.unstaged_files)
            .any(|file| !crate::consistency::is_generated_output(&file.path))
            || packet
                .git
                .untracked_files
                .iter()
                .any(|path| !crate::consistency::is_generated_output(path))
            || !packet.git.conflicted_files.is_empty(),
        point: RepositoryPoint {
            token: packet.consistency,
            tree_oid,
            branch: packet.git.branch,
        },
        changed_paths: packet.changed_surface,
    })
}

fn verify_packet_is_current(
    runner: &impl CommandRunner,
    cwd: &Path,
    packet: &ContextPacket,
) -> Result<()> {
    let (mut live, _) = crate::git::collect_git_state(runner, cwd)
        .context("failed to verify current Toimija state")?;
    crate::context::normalize_git_state(&mut live);
    match packet.consistency.verify(&live) {
        Verdict::Consistent => Ok(()),
        Verdict::Stale { .. } => bail!("repository changed while current state was collected"),
        Verdict::UnsupportedConsistencyAlgorithm(algorithm) => {
            bail!("unsupported Toimija consistency algorithm `{algorithm}`")
        }
        Verdict::UnsafeGitOperation(operation) => {
            bail!("repository is in an unsafe Git operation: {operation:?}")
        }
    }
}

fn reject_mixed_index_and_worktree(packet: &ContextPacket) -> Result<()> {
    let staged: BTreeSet<_> = packet
        .git
        .staged_files
        .iter()
        .map(|file| &file.path)
        .collect();
    let mixed: Vec<_> = packet
        .git
        .unstaged_files
        .iter()
        .filter(|file| staged.contains(&file.path))
        .map(|file| file.path.display().to_string())
        .collect();
    if !mixed.is_empty() {
        bail!(
            "cannot identify one current tree: staged and worktree content differ for {}",
            mixed.join(", ")
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeObservation {
    Known(Vec<PathBuf>),
    Unknown(String),
}

pub fn changes_between(
    runner: &impl CommandRunner,
    repo_root: &Path,
    origin: &TreeOid,
    current: &TreeOid,
) -> ScopeObservation {
    if origin == current {
        return ScopeObservation::Known(Vec::new());
    }
    match runner.run(
        repo_root,
        "git",
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "-z",
            &origin.0,
            &current.0,
        ],
    ) {
        Ok(output) if output.status == 0 => ScopeObservation::Known(
            output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(crate::path_codec::from_git_bytes)
                .collect(),
        ),
        Ok(output) => ScopeObservation::Unknown(format!("git diff-tree exited {}", output.status)),
        Err(error) => ScopeObservation::Unknown(format!("git diff-tree failed: {error}")),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ScopeStatus {
    WithinScope,
    Violated {
        #[serde(with = "crate::path_codec::vec")]
        paths: Vec<PathBuf>,
    },
    Unknown {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObligationStatus {
    pub id: ObligationId,
    pub description: String,
    pub resolved: bool,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GateStatus {
    Current,
    Stale { tree_oid: TreeOid },
    Failed { exit_status: i32 },
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateEvaluation {
    pub id: GateId,
    pub command: GateCommand,
    pub status: GateStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Blocker {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NextAction {
    pub action: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowStatus {
    pub scope: ScopeStatus,
    pub obligations: Vec<ObligationStatus>,
    pub gates: Vec<GateEvaluation>,
    pub current_completion: Option<CompletionLevel>,
    pub required_completion: CompletionLevel,
    pub completion_reachable: bool,
    pub blockers: Vec<Blocker>,
    pub next_actions: Vec<NextAction>,
    pub closable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub validation: bool,
}

impl Capabilities {
    pub fn for_spec(spec: &Spec) -> Self {
        Self {
            validation: !spec.verification.is_empty(),
        }
    }

    pub fn supports(self, level: CompletionLevel) -> bool {
        level == CompletionLevel::Changed || self.validation
    }
}

pub fn evaluate(
    spec: &Spec,
    work: &Work,
    current: &VerifiedRepository,
    evidence: &EvidenceSet,
    capabilities: Capabilities,
    scope_observation: ScopeObservation,
) -> WorkflowStatus {
    let scope = evaluate_scope(&work.scope, scope_observation);
    let gates: Vec<_> = spec
        .verification
        .iter()
        .map(|gate| GateEvaluation {
            id: gate.id.clone(),
            command: gate.command.clone(),
            status: gate_status(work, gate, current, evidence),
        })
        .collect();
    let claims: BTreeSet<_> = evidence
        .manual_claims
        .iter()
        .filter(|claim| claim.work_id == work.id)
        .map(|claim| claim.obligation_id.clone())
        .collect();
    let changed = work.origin.tree_oid != current.point.tree_oid;
    let obligations = work
        .obligations
        .iter()
        .map(|obligation| {
            let (resolved, evidence) = match &obligation.kind {
                ObligationKind::Manual => (
                    claims.contains(&obligation.id),
                    "explicit claim".to_string(),
                ),
                ObligationKind::Gate { gate_id } => {
                    let current = gates.iter().any(|gate| {
                        gate.id == *gate_id && matches!(gate.status, GateStatus::Current)
                    });
                    (current, "tree-bound gate receipt".to_string())
                }
                ObligationKind::Scope => (
                    matches!(scope, ScopeStatus::WithinScope),
                    "Toimija tree diff".to_string(),
                ),
                ObligationKind::RepositoryChanged => {
                    (changed, "origin/current tree comparison".to_string())
                }
            };
            ObligationStatus {
                id: obligation.id.clone(),
                description: obligation.description.clone(),
                resolved,
                evidence,
            }
        })
        .collect::<Vec<_>>();
    let gates_current = gates
        .iter()
        .all(|gate| matches!(gate.status, GateStatus::Current));
    let current_completion = if changed && gates_current && !gates.is_empty() {
        Some(CompletionLevel::Validated)
    } else if changed {
        Some(CompletionLevel::Changed)
    } else {
        None
    };
    let completion_reachable = capabilities.supports(work.completion);
    let mut blockers = Vec::new();
    match &scope {
        ScopeStatus::WithinScope => {}
        ScopeStatus::Violated { paths } => blockers.push(Blocker {
            code: "scope-violated".to_string(),
            message: format!(
                "changed paths are outside scope: {}",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
        ScopeStatus::Unknown { reason } => blockers.push(Blocker {
            code: "scope-unknown".to_string(),
            message: format!("scope could not be determined: {reason}"),
        }),
    }
    for obligation in obligations.iter().filter(|item| !item.resolved) {
        let message = work
            .obligations
            .iter()
            .find(|candidate| candidate.id == obligation.id)
            .and_then(|candidate| match &candidate.kind {
                ObligationKind::Gate { gate_id } => gates
                    .iter()
                    .find(|gate| gate.id == *gate_id)
                    .map(|gate| match &gate.status {
                        GateStatus::Current => unreachable!("resolved gate was filtered out"),
                        GateStatus::Stale { tree_oid } => format!(
                            "{} passed on tree {}, current tree is {}",
                            gate.command.shell, tree_oid, current.point.tree_oid
                        ),
                        GateStatus::Failed { exit_status } => {
                            format!("{} failed with exit {exit_status}", gate.command.shell)
                        }
                        GateStatus::Missing => {
                            format!("{} has not passed on the current tree", gate.command.shell)
                        }
                    }),
                _ => None,
            })
            .unwrap_or_else(|| {
                format!("obligation `{}` remains unresolved", obligation.description)
            });
        blockers.push(Blocker {
            code: format!("obligation-{}", obligation.id),
            message,
        });
    }
    if !completion_reachable {
        blockers.push(Blocker {
            code: "completion-unreachable".to_string(),
            message: format!(
                "{} completion is unreachable without an executable gate",
                work.completion
            ),
        });
    } else if current_completion < Some(work.completion) {
        blockers.push(Blocker {
            code: "completion-incomplete".to_string(),
            message: format!("{} completion is required", work.completion),
        });
    }
    let next_actions = blockers
        .first()
        .map(|blocker| NextAction {
            action: next_action(blocker, &gates),
            reason: blocker.message.clone(),
        })
        .into_iter()
        .collect();
    WorkflowStatus {
        scope,
        obligations,
        gates,
        current_completion,
        required_completion: work.completion,
        completion_reachable,
        closable: blockers.is_empty(),
        blockers,
        next_actions,
    }
}

fn evaluate_scope(scope: &Scope, observation: ScopeObservation) -> ScopeStatus {
    match observation {
        ScopeObservation::Unknown(reason) => ScopeStatus::Unknown { reason },
        ScopeObservation::Known(paths) => {
            let outside = paths
                .into_iter()
                .filter(|path| !scope.write.iter().any(|rule| rule.matches(path)))
                .collect::<Vec<_>>();
            if outside.is_empty() {
                ScopeStatus::WithinScope
            } else {
                ScopeStatus::Violated { paths: outside }
            }
        }
    }
}

fn gate_status(
    work: &Work,
    gate: &GateRequirement,
    current: &VerifiedRepository,
    evidence: &EvidenceSet,
) -> GateStatus {
    let latest = evidence
        .gate_receipts
        .iter()
        .rev()
        .find(|receipt| receipt.work_id == work.id && receipt.gate_id == gate.id);
    match latest {
        Some(receipt) if receipt.exit_status != 0 => GateStatus::Failed {
            exit_status: receipt.exit_status,
        },
        Some(receipt) if receipt.tree_oid == current.point.tree_oid => GateStatus::Current,
        Some(receipt) => GateStatus::Stale {
            tree_oid: receipt.tree_oid.clone(),
        },
        None => GateStatus::Missing,
    }
}

fn next_action(blocker: &Blocker, gates: &[GateEvaluation]) -> String {
    if blocker.code.starts_with("obligation-")
        && let Some(gate) = gates.iter().find(|gate| {
            !matches!(gate.status, GateStatus::Current) && blocker.code.ends_with(&gate.id.0)
        })
    {
        return format!("run {}", gate.command.shell);
    }
    if blocker.code == "completion-incomplete"
        && let Some(gate) = gates
            .iter()
            .find(|gate| !matches!(gate.status, GateStatus::Current))
    {
        return format!("run {}", gate.command.shell);
    }
    match blocker.code.as_str() {
        "scope-violated" => "restore or move out-of-scope changes".to_string(),
        "scope-unknown" => "restore verifiable repository state".to_string(),
        code if code.contains("repository-changed") => {
            "make the bounded repository change".to_string()
        }
        code if code.starts_with("obligation-") => {
            format!(
                "resolve and claim {}",
                code.trim_start_matches("obligation-")
            )
        }
        _ => "resolve the first blocker".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consistency::{ConflictState, HeadState};

    fn consistency(token: &str) -> Consistency {
        Consistency {
            algorithm: crate::consistency::CONSISTENCY_ALGORITHM.to_string(),
            head: HeadState::Normal {
                branch: "main".to_string(),
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
            token: token.to_string(),
        }
    }

    fn fixture() -> (Spec, Work, VerifiedRepository) {
        let gate = GateRequirement {
            id: GateId("test".to_string()),
            command: GateCommand {
                shell: "cargo test".to_string(),
            },
        };
        let spec = Spec {
            id: SpecId("spec".to_string()),
            title: "Teko".to_string(),
            objective: "be honest".to_string(),
            goals: vec![Goal {
                id: GoalId("honesty".to_string()),
                description: "refuse false closure".to_string(),
            }],
            non_goals: Vec::new(),
            invariants: Vec::new(),
            verification: vec![gate],
            completion: CompletionLevel::Validated,
            status: SpecStatus::Ratified,
            source: None,
        };
        let work = Work {
            id: WorkId("work".to_string()),
            spec_id: spec.id.clone(),
            objective: spec.objective.clone(),
            scope: Scope {
                write: vec![PathRule::new(PathBuf::from("src")).unwrap()],
            },
            obligations: vec![
                Obligation {
                    id: ObligationId("honesty".to_string()),
                    description: "refuse false closure".to_string(),
                    kind: ObligationKind::Manual,
                },
                Obligation {
                    id: ObligationId("gate-test".to_string()),
                    description: "cargo test passes".to_string(),
                    kind: ObligationKind::Gate {
                        gate_id: GateId("test".to_string()),
                    },
                },
                Obligation {
                    id: ObligationId("scope".to_string()),
                    description: "changes stay in scope".to_string(),
                    kind: ObligationKind::Scope,
                },
                Obligation {
                    id: ObligationId("repository-changed".to_string()),
                    description: "repository changed".to_string(),
                    kind: ObligationKind::RepositoryChanged,
                },
            ],
            completion: CompletionLevel::Validated,
            origin: RepositoryPoint {
                token: consistency("a"),
                tree_oid: TreeOid("tree-a".to_string()),
                branch: Some("main".to_string()),
            },
            status: WorkStatus::Active,
        };
        let current = VerifiedRepository {
            point: RepositoryPoint {
                token: consistency("b"),
                tree_oid: TreeOid("tree-b".to_string()),
                branch: Some("main".to_string()),
            },
            changed_paths: vec![PathBuf::from("src/lib.rs")],
            dirty: true,
        };
        (spec, work, current)
    }

    fn receipt(work: &Work, tree: &str) -> GateReceipt {
        GateReceipt {
            work_id: work.id.clone(),
            gate_id: GateId("test".to_string()),
            tree_oid: TreeOid(tree.to_string()),
            command: GateCommand {
                shell: "cargo test".to_string(),
            },
            exit_status: 0,
            output_digest: "sha256:x".to_string(),
            passed_at: 1,
        }
    }

    #[test]
    fn closure_is_derived_from_current_tree_evidence() {
        let (spec, work, current) = fixture();
        let evidence = EvidenceSet {
            gate_receipts: vec![receipt(&work, "tree-b")],
            manual_claims: vec![ManualClaim {
                work_id: work.id.clone(),
                obligation_id: ObligationId("honesty".to_string()),
                claimed_at: 1,
            }],
            completion_witness: None,
        };
        let status = evaluate(
            &spec,
            &work,
            &current,
            &evidence,
            Capabilities::for_spec(&spec),
            ScopeObservation::Known(vec![PathBuf::from("src/lib.rs")]),
        );
        assert!(status.closable);
        assert_eq!(status.current_completion, Some(CompletionLevel::Validated));
    }

    #[test]
    fn old_tree_receipt_is_historical_but_stale() {
        let (spec, work, current) = fixture();
        let evidence = EvidenceSet {
            gate_receipts: vec![receipt(&work, "tree-a")],
            manual_claims: vec![ManualClaim {
                work_id: work.id.clone(),
                obligation_id: ObligationId("honesty".to_string()),
                claimed_at: 1,
            }],
            completion_witness: None,
        };
        let status = evaluate(
            &spec,
            &work,
            &current,
            &evidence,
            Capabilities::for_spec(&spec),
            ScopeObservation::Known(vec![PathBuf::from("src/lib.rs")]),
        );
        assert!(!status.closable);
        assert!(matches!(status.gates[0].status, GateStatus::Stale { .. }));
        assert_eq!(evidence.gate_receipts.len(), 1);
    }

    #[test]
    fn scope_unknown_and_violation_both_fail_closed() {
        let (spec, work, current) = fixture();
        for observation in [
            ScopeObservation::Unknown("git failed".to_string()),
            ScopeObservation::Known(vec![PathBuf::from("README.md")]),
        ] {
            let status = evaluate(
                &spec,
                &work,
                &current,
                &EvidenceSet::default(),
                Capabilities::for_spec(&spec),
                observation,
            );
            assert!(!status.closable);
            assert!(!matches!(status.scope, ScopeStatus::WithinScope));
        }
    }

    #[test]
    fn a_claim_for_another_work_cannot_resolve_an_obligation() {
        let (spec, work, current) = fixture();
        let evidence = EvidenceSet {
            gate_receipts: vec![receipt(&work, "tree-b")],
            manual_claims: vec![ManualClaim {
                work_id: WorkId("other".to_string()),
                obligation_id: ObligationId("honesty".to_string()),
                claimed_at: 1,
            }],
            completion_witness: None,
        };
        let status = evaluate(
            &spec,
            &work,
            &current,
            &evidence,
            Capabilities::for_spec(&spec),
            ScopeObservation::Known(vec![PathBuf::from("src/lib.rs")]),
        );
        assert!(!status.closable);
    }

    #[test]
    fn validated_work_without_a_gate_is_unreachable() {
        let (mut spec, work, current) = fixture();
        spec.verification.clear();
        let status = evaluate(
            &spec,
            &work,
            &current,
            &EvidenceSet::default(),
            Capabilities::for_spec(&spec),
            ScopeObservation::Known(vec![PathBuf::from("src/lib.rs")]),
        );
        assert!(!status.completion_reachable);
        assert!(!status.closable);
    }
}
