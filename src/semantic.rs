use std::path::Path;

use anyhow::{Context, Result, bail};
use false_agent_protocol::{
    BranchName, ChangedPath, CompletionLevel, ConsistencyToken, EntityRef, Extensions, FactSource,
    Product, ProtocolVersion, Relation, RepositoryPoint, RepositoryPointEnvelope,
    RepositoryPointRef, RepositoryRef, SemanticFact, SemanticProjection, Timestamp, TreeOid,
    context::{ExecutionContextEnvelope, RepositoryCapabilities},
};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use tracing::info;

use crate::command::CommandRunner;
use crate::context::ContextPacket;
use crate::teko::ActiveWork;

pub const CONTEXT_FILE: &str = "semantic-context.json";
pub const RESULT_FILE: &str = "semantic-result.json";
pub const POINT_FILE: &str = "semantic-point.json";

pub fn execution_context(
    runner: &impl CommandRunner,
    context: &ContextPacket,
    active: Option<ActiveWork>,
    generation: u64,
) -> Result<ExecutionContextEnvelope> {
    let repository = repository_point(runner, context)?;
    let mut reachable_completion = vec![CompletionLevel::Changed];
    if active
        .as_ref()
        .is_some_and(|active| !active.packet.gates.is_empty())
    {
        reachable_completion.push(CompletionLevel::Validated);
    }
    let work = active.as_ref().map(|active| active.packet.clone());
    let workflow_status = active.map(|active| active.status);
    let mut envelope = ExecutionContextEnvelope {
        protocol: ProtocolVersion::V1,
        generated_at: now_timestamp(),
        generation,
        repository,
        changed_surfaces: context
            .changed_surface
            .iter()
            .map(|path| ChangedPath {
                path: path.display().to_string(),
                status: None,
            })
            .collect(),
        repository_capabilities: RepositoryCapabilities {
            reachable_completion,
        },
        work,
        workflow_status,
        context_digest: String::new(),
        extensions: Extensions::new(),
    };
    envelope.seal().map_err(anyhow::Error::from)?;
    info!(
        event = "toimija_context_generated",
        work = envelope.work.as_ref().map(|work| work.work.id.to_string()),
        repository_point = %envelope.repository.id,
        generation = envelope.generation,
        protocol_major = envelope.protocol.major,
        protocol_minor = envelope.protocol.minor
    );
    Ok(envelope)
}

pub fn repository_point(
    runner: &impl CommandRunner,
    context: &ContextPacket,
) -> Result<RepositoryPoint> {
    let tree = crate::snapshot::materialize(runner, &context.git.repo_root)
        .context("failed to materialize the authoritative repository point")?;
    let repository = repository_ref(&context.git.repo_root)?;
    let id = point_ref(&repository, &context.consistency.token)?;
    Ok(RepositoryPoint {
        id,
        repository,
        consistency_token: ConsistencyToken(context.consistency.token.clone()),
        tree_oid: TreeOid(tree),
        branch: context.git.branch.clone().map(BranchName),
        dirty: context.git.dirty,
        captured_at: now_timestamp(),
    })
}

pub fn point_envelope(point: RepositoryPoint) -> RepositoryPointEnvelope {
    RepositoryPointEnvelope {
        protocol: ProtocolVersion::V1,
        repository: point,
        extensions: Extensions::new(),
    }
}

pub fn render_execution(context: &ExecutionContextEnvelope) -> String {
    let Some(work) = context.work.as_ref() else {
        return "# Unbound Work\n\nNo active work provider.".into();
    };
    let status = context.workflow_status.as_ref();
    let mut lines = vec![
        "# Active Teko Work".to_string(),
        String::new(),
        format!("Work: `{}`", work.work.id),
        format!("Spec: `{}` — {}", work.spec.id, work.spec.title),
        format!("Objective: {}", work.work.objective),
        format!("Repository point: `{}`", context.repository.id),
        "Obligations:".to_string(),
    ];
    lines.extend(work.obligations.iter().map(|obligation| {
        let supported = status
            .and_then(|status| {
                status
                    .obligations
                    .iter()
                    .find(|state| state.obligation == obligation.id)
            })
            .is_some_and(|state| state.supported);
        format!(
            "- [{}] `{}` {}",
            if supported { "done" } else { "open" },
            obligation.id,
            obligation.description
        )
    }));
    let blockers = status
        .map(|status| status.blockers.as_slice())
        .unwrap_or(&[]);
    if blockers.is_empty() {
        lines.push("Closure blockers: none".to_string());
    } else {
        lines.push("Closure blockers:".to_string());
        lines.extend(
            blockers
                .iter()
                .map(|blocker| format!("- `{}`: {}", blocker.code, blocker.message)),
        );
    }
    lines.join("\n")
}

pub fn render_teko_status(json: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return "Teko imported evidence; run `teko show` for current status.".to_string();
    };
    if value.pointer("/status/closable").and_then(Value::as_bool) == Some(true) {
        return "Teko imported evidence; work is ready to close.".to_string();
    }
    let blockers = value
        .pointer("/status/blockers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|blocker| {
            format!(
                "- [{}] {}",
                blocker["code"].as_str().unwrap_or("unknown"),
                blocker["message"].as_str().unwrap_or("unknown")
            )
        })
        .collect::<Vec<_>>();
    if blockers.is_empty() {
        "Teko imported evidence; work remains open.".to_string()
    } else {
        format!(
            "Teko imported evidence; remaining:\n{}",
            blockers.join("\n")
        )
    }
}

pub fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    crate::atomic_write::replace(path, &bytes)
        .with_context(|| format!("failed to write {}", path.display()))
}

pub fn read_result(path: &Path) -> Result<false_agent_protocol::WorkerRunResultEnvelope> {
    const MAX_BYTES: u64 = 1024 * 1024;
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("refusing Kisko result through symlink {}", path.display());
    }
    if metadata.len() > MAX_BYTES {
        bail!("Kisko result exceeds the {MAX_BYTES}-byte limit");
    }
    let result = serde_json::from_slice::<false_agent_protocol::WorkerRunResultEnvelope>(
        &std::fs::read(path)?,
    )
    .context("Kisko result is not a valid worker-result envelope")?;
    result.validate().map_err(anyhow::Error::from)?;
    Ok(result)
}

#[derive(Debug)]
pub struct Correlation {
    pub result: false_agent_protocol::WorkerRunResultEnvelope,
    pub durable_path: std::path::PathBuf,
    pub teko_status: Option<String>,
}

pub trait WorkObserver {
    fn observe(
        &self,
        repo_root: &Path,
        point: &RepositoryPoint,
        result: false_agent_protocol::WorkerRunResultEnvelope,
        outcome: false_agent_protocol::work::ProcessOutcome,
    ) -> Result<String>;
}

pub struct RuntimeWorkObserver;

impl WorkObserver for RuntimeWorkObserver {
    fn observe(
        &self,
        repo_root: &Path,
        point: &RepositoryPoint,
        result: false_agent_protocol::WorkerRunResultEnvelope,
        outcome: false_agent_protocol::work::ProcessOutcome,
    ) -> Result<String> {
        let (provider, _) = crate::teko::WorkProvider::open(repo_root, point, true)?;
        provider
            .context("work provider disappeared before observation")?
            .lock()
            .map_err(|_| anyhow::anyhow!("work provider session lock was poisoned"))?
            .observe_result(point, result, outcome)
    }
}

pub struct SessionWorkObserver {
    pub provider: Option<crate::teko::SharedWorkProvider>,
}

impl WorkObserver for SessionWorkObserver {
    fn observe(
        &self,
        _repo_root: &Path,
        point: &RepositoryPoint,
        result: false_agent_protocol::WorkerRunResultEnvelope,
        outcome: false_agent_protocol::work::ProcessOutcome,
    ) -> Result<String> {
        self.provider
            .as_ref()
            .context("bound result has no initialized work provider")?
            .lock()
            .map_err(|_| anyhow::anyhow!("work provider session lock was poisoned"))?
            .observe_result(point, result, outcome)
    }
}

pub fn correlate_result(
    observer: &impl WorkObserver,
    repo_root: &Path,
    entry: Option<&ExecutionContextEnvelope>,
    exit: Option<&ExecutionContextEnvelope>,
    result_path: &Path,
    accepted_context_digests: &std::collections::BTreeSet<String>,
    outcome: false_agent_protocol::work::ProcessOutcome,
) -> Result<Option<Correlation>> {
    if !result_path.is_file() {
        return Ok(None);
    }
    let mut result = read_result(result_path)?;
    let teko_status = match entry.and_then(|entry| entry.work.as_ref().map(|work| (entry, work))) {
        Some((entry, entry_work)) => {
            let exit = exit.context(
                "Kisko returned bound evidence, but Toimija could not refresh current repository truth",
            )?;
            let exit_work = exit
                .work
                .as_ref()
                .context("active work disappeared while the bound worker was running")?;
            let binding = result.binding.as_ref().context(
                "Kisko result is unbound, but this run launched with authoritative Teko work",
            )?;
            if binding.work != entry_work.work.id
                || binding.spec != entry_work.spec.id
                || binding.origin != entry.repository.id
                || !accepted_context_digests.contains(&binding.context_digest)
                || result.origin != entry.repository.id
            {
                bail!(
                    "Cannot attach Kisko run {} to work {}. Expected work {} implementing {} at {} with one of {} emitted context digests; result references work {} implementing {} at {} with digest {}. Teko is authoritative for active work and Toimija for emitted context; restart Kisko through Toimija with the current context.",
                    result.run,
                    binding.work,
                    entry_work.work.id,
                    entry_work.spec.id,
                    entry.repository.id,
                    accepted_context_digests.len(),
                    binding.work,
                    binding.spec,
                    result.origin,
                    binding.context_digest
                );
            }
            if exit_work.work.id != entry_work.work.id {
                bail!(
                    "Active Teko work changed from {} to {} while Kisko ran. Teko is authoritative; restart the run with the current work.",
                    entry_work.work.id,
                    exit_work.work.id
                );
            }
            let mut changed = false;
            match &result.destination {
                Some(destination) if destination != &exit.repository.id => bail!(
                    "Kisko result claims destination {}, but Toimija verified {}. Toimija is authoritative for repository truth; refresh and emit a result for the verified point.",
                    destination,
                    exit.repository.id
                ),
                None => {
                    result.destination = Some(exit.repository.id.clone());
                    changed = true;
                }
                Some(_) => {}
            }
            for action in &mut result.actions {
                let transforms = matches!(
                    action.kind,
                    false_agent_protocol::ActionKind::WriteFile
                        | false_agent_protocol::ActionKind::ApplyPatch
                        | false_agent_protocol::ActionKind::RevertBatch
                        | false_agent_protocol::ActionKind::Commit
                );
                if !transforms {
                    continue;
                }
                if action
                    .transforms_from
                    .as_ref()
                    .is_some_and(|point| point != &entry.repository.id)
                {
                    bail!(
                        "Kisko action {} claims origin {:?}, but Toimija launched at {}. Toimija is authoritative for repository truth.",
                        action.id,
                        action.transforms_from,
                        entry.repository.id
                    );
                }
                if action
                    .transforms_to
                    .as_ref()
                    .is_some_and(|point| point != &exit.repository.id)
                {
                    bail!(
                        "Kisko action {} claims destination {:?}, but Toimija verified {}. Toimija is authoritative for repository truth.",
                        action.id,
                        action.transforms_to,
                        exit.repository.id
                    );
                }
                if action.transforms_from.is_none() {
                    action.transforms_from = Some(entry.repository.id.clone());
                    changed = true;
                }
                if action.transforms_to.is_none() {
                    action.transforms_to = Some(exit.repository.id.clone());
                    changed = true;
                }
            }
            if changed {
                result.seal().map_err(anyhow::Error::from)?;
                write_json(result_path, &result)?;
            }
            let point_path = result_path.with_file_name("semantic-point.json");
            write_json(&point_path, &point_envelope(exit.repository.clone()))?;
            let status = observer.observe(repo_root, &exit.repository, result.clone(), outcome)?;
            info!(
                event = "teko_evidence_imported",
                run = %result.run,
                work = %entry_work.work.id,
                repository_point = %exit.repository.id,
                protocol_major = result.protocol.major,
                protocol_minor = result.protocol.minor
            );
            Some(status)
        }
        None => {
            if result.binding.is_some() {
                bail!(
                    "Kisko result {} claims bound work, but Toimija launched no active Teko context. Teko is authoritative; restart through Toimija with active work.",
                    result.run
                );
            }
            None
        }
    };

    let durable_path = repo_root
        .join(".toimija/runs")
        .join(format!("{}.result.json", result.run));
    if let Some(parent) = durable_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    write_json(&durable_path, &result)?;
    if durable_path != result_path {
        let _ = std::fs::remove_file(result_path);
    }
    info!(
        event = "run_result_correlated",
        run = %result.run,
        destination = ?result.destination,
        protocol_major = result.protocol.major,
        protocol_minor = result.protocol.minor
    );
    Ok(Some(Correlation {
        result,
        durable_path,
        teko_status,
    }))
}

pub fn latest_result(
    repo_root: &Path,
) -> Result<Option<false_agent_protocol::WorkerRunResultEnvelope>> {
    let dir = repo_root.join(".toimija/runs");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", dir.display()));
        }
    };
    let mut candidates = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".result.json"))
        })
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    candidates
        .first()
        .map(|(_, path)| read_result(path))
        .transpose()
}

pub fn projection(
    context: &ExecutionContextEnvelope,
    result: Option<&false_agent_protocol::WorkerRunResultEnvelope>,
) -> Result<SemanticProjection> {
    let Some(packet) = context.work.as_ref() else {
        return SemanticProjection::derive([]).map_err(anyhow::Error::from);
    };
    let teko = |source_entity| FactSource {
        product: Product::Teko,
        source_entity,
        event_sequence: None,
    };
    let kisko = |source_entity, event_sequence| FactSource {
        product: Product::Kisko,
        source_entity,
        event_sequence,
    };
    let work = EntityRef::Work(packet.work.id.clone());
    let mut facts = vec![SemanticFact {
        subject: work.clone(),
        relation: Relation::Implements,
        object: EntityRef::Spec(packet.spec.id.clone()),
        source: teko(Some(work.clone())),
        evidence: Vec::new(),
    }];
    facts.extend(packet.obligations.iter().map(|obligation| SemanticFact {
        subject: work.clone(),
        relation: Relation::Requires,
        object: EntityRef::Obligation(obligation.id.clone()),
        source: teko(Some(work.clone())),
        evidence: Vec::new(),
    }));
    for state in context
        .workflow_status
        .iter()
        .flat_map(|status| &status.obligations)
    {
        for evidence in &state.evidence {
            facts.push(SemanticFact {
                subject: EntityRef::Evidence(evidence.clone()),
                relation: Relation::Supports,
                object: EntityRef::Obligation(state.obligation.clone()),
                source: teko(Some(EntityRef::Evidence(evidence.clone()))),
                evidence: vec![evidence.clone()],
            });
            if let Some(receipt) = evidence
                .as_str()
                .strip_prefix("ev_")
                .and_then(|value| value.parse::<false_agent_protocol::GateReceiptRef>().ok())
            {
                facts.extend([
                    SemanticFact {
                        subject: EntityRef::GateReceipt(receipt.clone()),
                        relation: Relation::Verifies,
                        object: EntityRef::RepositoryPoint(context.repository.id.clone()),
                        source: teko(Some(EntityRef::GateReceipt(receipt.clone()))),
                        evidence: vec![evidence.clone()],
                    },
                    SemanticFact {
                        subject: EntityRef::GateReceipt(receipt.clone()),
                        relation: Relation::Supports,
                        object: EntityRef::Obligation(state.obligation.clone()),
                        source: teko(Some(EntityRef::GateReceipt(receipt))),
                        evidence: vec![evidence.clone()],
                    },
                ]);
            }
        }
    }
    facts.extend(packet.gates.iter().map(|gate| SemanticFact {
        subject: work.clone(),
        relation: Relation::Requires,
        object: EntityRef::Gate(gate.id.clone()),
        source: teko(Some(work.clone())),
        evidence: Vec::new(),
    }));

    if let Some(result) = result
        && let Some(binding) = &result.binding
        && binding.work == packet.work.id
    {
        let run = EntityRef::WorkerRun(result.run.clone());
        facts.extend([
            SemanticFact {
                subject: work.clone(),
                relation: Relation::BoundTo,
                object: EntityRef::RepositoryPoint(binding.origin.clone()),
                source: teko(Some(work.clone())),
                evidence: Vec::new(),
            },
            SemanticFact {
                subject: run.clone(),
                relation: Relation::Executes,
                object: work.clone(),
                source: kisko(Some(run.clone()), None),
                evidence: Vec::new(),
            },
            SemanticFact {
                subject: run.clone(),
                relation: Relation::TransformsFrom,
                object: EntityRef::RepositoryPoint(result.origin.clone()),
                source: kisko(Some(run.clone()), None),
                evidence: Vec::new(),
            },
        ]);
        if let Some(destination) = &result.destination {
            facts.push(SemanticFact {
                subject: run.clone(),
                relation: Relation::TransformsTo,
                object: EntityRef::RepositoryPoint(destination.clone()),
                source: kisko(Some(run.clone()), None),
                evidence: Vec::new(),
            });
        }
        for (sequence, action) in result.actions.iter().enumerate() {
            let action_ref = EntityRef::Action(action.id.clone());
            facts.push(SemanticFact {
                subject: run.clone(),
                relation: Relation::ContainsAction,
                object: action_ref.clone(),
                source: kisko(Some(run.clone()), Some(sequence as u64)),
                evidence: action.evidence.clone(),
            });
            if let Some(tool) = &action.tool_call {
                facts.push(SemanticFact {
                    subject: action_ref.clone(),
                    relation: Relation::CausedBy,
                    object: EntityRef::ToolCall(tool.clone()),
                    source: kisko(Some(action_ref.clone()), Some(sequence as u64)),
                    evidence: action.evidence.clone(),
                });
            }
            if let Some(point) = &action.transforms_from {
                facts.push(SemanticFact {
                    subject: action_ref.clone(),
                    relation: Relation::TransformsFrom,
                    object: EntityRef::RepositoryPoint(point.clone()),
                    source: kisko(Some(action_ref.clone()), Some(sequence as u64)),
                    evidence: action.evidence.clone(),
                });
            }
            if let Some(point) = &action.transforms_to {
                facts.push(SemanticFact {
                    subject: action_ref.clone(),
                    relation: Relation::TransformsTo,
                    object: EntityRef::RepositoryPoint(point.clone()),
                    source: kisko(Some(action_ref.clone()), Some(sequence as u64)),
                    evidence: action.evidence.clone(),
                });
            }
        }
        for evidence in &result.evidence {
            facts.extend(evidence.supports.iter().map(|obligation| SemanticFact {
                subject: EntityRef::Evidence(evidence.id.clone()),
                relation: Relation::Supports,
                object: EntityRef::Obligation(obligation.clone()),
                source: kisko(Some(EntityRef::Evidence(evidence.id.clone())), None),
                evidence: vec![evidence.id.clone()],
            }));
        }
    }
    SemanticProjection::derive(facts).map_err(anyhow::Error::from)
}

fn repository_ref(root: &Path) -> Result<RepositoryRef> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    RepositoryRef::parse(format!(
        "repo_{:x}",
        Sha256::digest(root.as_os_str().as_encoded_bytes())
    ))
    .map_err(anyhow::Error::from)
}

fn point_ref(repository: &RepositoryRef, consistency_token: &str) -> Result<RepositoryPointRef> {
    RepositoryPointRef::parse(format!(
        "rpoint_{:x}",
        Sha256::digest(format!("{repository}\0{consistency_token}"))
    ))
    .map_err(anyhow::Error::from)
}

fn now_timestamp() -> Timestamp {
    Timestamp(
        OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ObserveOk;

    impl WorkObserver for ObserveOk {
        fn observe(
            &self,
            _repo_root: &Path,
            _point: &RepositoryPoint,
            _result: false_agent_protocol::WorkerRunResultEnvelope,
            _outcome: false_agent_protocol::work::ProcessOutcome,
        ) -> Result<String> {
            Ok(r#"{"status":{"closable":false,"blockers":[]}}"#.into())
        }
    }

    fn bound_context() -> ExecutionContextEnvelope {
        let legacy: false_agent_protocol::ExecutionContextEnvelope =
            serde_json::from_str(false_agent_protocol::EXECUTION_CONTEXT_FIXTURE).unwrap();
        let mut context = ExecutionContextEnvelope {
            protocol: legacy.protocol,
            generation: legacy.generation,
            generated_at: legacy.generated_at,
            repository: legacy.repository,
            changed_surfaces: legacy.changed_paths,
            repository_capabilities: false_agent_protocol::context::RepositoryCapabilities {
                reachable_completion: legacy.capabilities.reachable_completion,
            },
            work: Some(legacy.work),
            workflow_status: Some(legacy.workflow_status),
            context_digest: String::new(),
            extensions: legacy.extensions,
        };
        context.seal().unwrap();
        context
    }

    #[test]
    fn repository_and_point_refs_are_typed_and_deterministic() {
        let repository = repository_ref(Path::new(".")).unwrap();
        assert!(repository.as_str().starts_with("repo_"));
        assert_eq!(
            point_ref(&repository, "token").unwrap(),
            point_ref(&repository, "token").unwrap()
        );
        assert_ne!(
            point_ref(&repository, "token").unwrap(),
            point_ref(&repository, "other").unwrap()
        );
    }

    #[test]
    fn shared_fixtures_are_consumed_from_the_protocol_crate() {
        let context = bound_context();
        context.validate().unwrap();
        let unbound: ExecutionContextEnvelope =
            serde_json::from_str(false_agent_protocol::UNBOUND_CONTEXT_FIXTURE).unwrap();
        assert_eq!(
            unbound.context_digest,
            false_agent_protocol::digest::sha256_without(&unbound, "context_digest").unwrap()
        );
        unbound.validate().unwrap();
        let result: false_agent_protocol::WorkerRunResultEnvelope =
            serde_json::from_str(false_agent_protocol::WORKER_RESULT_FIXTURE).unwrap();
        result.validate().unwrap();
    }

    #[test]
    fn correlation_replaces_missing_destination_with_toimija_truth() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".toimija")).unwrap();
        let mut entry = bound_context();
        let mut exit = entry.clone();
        exit.repository.id = "rpoint_fixture-b".parse().unwrap();
        exit.repository.consistency_token = ConsistencyToken("sha256:context-b".into());
        exit.repository.tree_oid = TreeOid("2222222222222222222222222222222222222222".into());
        exit.seal().unwrap();

        let mut result: false_agent_protocol::WorkerRunResultEnvelope =
            serde_json::from_str(false_agent_protocol::WORKER_RESULT_FIXTURE).unwrap();
        let binding = result.binding.as_mut().unwrap();
        binding.context_digest = entry.context_digest.clone();
        result.destination = None;
        result.actions[0].transforms_to = None;
        result.seal().unwrap();
        let result_path = dir.path().join(".toimija/result.json");
        write_json(&result_path, &result).unwrap();

        let correlated = correlate_result(
            &ObserveOk,
            dir.path(),
            Some(&entry),
            Some(&exit),
            &result_path,
            &std::iter::once(entry.context_digest.clone()).collect(),
            false_agent_protocol::work::ProcessOutcome::Exited { code: 0 },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            correlated.result.destination.as_ref(),
            Some(&exit.repository.id)
        );
        assert_eq!(
            correlated.result.actions[0].transforms_to.as_ref(),
            Some(&exit.repository.id)
        );
        correlated.result.validate().unwrap();

        entry.work.as_mut().unwrap().work.id = "work_other".parse().unwrap();
        entry.seal().unwrap();
        let mut mismatched = correlated.result.clone();
        mismatched.destination = None;
        mismatched.seal().unwrap();
        write_json(&result_path, &mismatched).unwrap();
        let error = correlate_result(
            &ObserveOk,
            dir.path(),
            Some(&entry),
            Some(&exit),
            &result_path,
            &std::iter::once(entry.context_digest.clone()).collect(),
            false_agent_protocol::work::ProcessOutcome::Exited { code: 0 },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("Cannot attach Kisko run"), "{error}");
    }
}
