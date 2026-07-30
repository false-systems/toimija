use std::path::Path;

use anyhow::{Context, Result, bail};
use false_agent_protocol::{
    RepositoryPoint, WorkPacketEnvelope, WorkRef, WorkerRunResultEnvelope, WorkflowStatusSummary,
    plugin::PluginKind,
    work::{
        CLOSE_METHOD, CURRENT_METHOD, CloseResult, CurrentWorkParams, CurrentWorkResult,
        EXPLAIN_METHOD, ExplainParams, ExplainResult, OBSERVE_METHOD, ObserveParams,
        ProcessOutcome,
    },
};
use serde::Serialize;

use crate::command::ProcessCommandRunner;
use crate::config::RepoConfig;
use crate::gates::{GateAuthority, GateCatalog, GateId};
use crate::plugins::{PluginSession, discover};

#[derive(Debug, Clone)]
pub struct ActiveWork {
    pub packet: WorkPacketEnvelope,
    pub status: WorkflowStatusSummary,
}

pub struct WorkProvider {
    plugin: PluginSession,
    repo_root: std::path::PathBuf,
    work: Option<WorkPacketEnvelope>,
}

const OBSERVE_GATES_METHOD: &str = "work.observe_gates";
const REQUIRED_GATES_EXTENSION: &str = "toimija.required_gates.v1";

#[derive(Serialize)]
struct ObserveGatesParams<'a> {
    work: WorkRef,
    repository_point: &'a RepositoryPoint,
    gate_authority: &'a GateAuthority,
    receipts: &'a [crate::gates::GateReceipt],
}

#[derive(Serialize)]
struct AuthorityParams<'a> {
    repository_point: &'a RepositoryPoint,
    gate_authority: &'a GateAuthority,
}

pub type SharedWorkProvider = std::sync::Arc<std::sync::Mutex<WorkProvider>>;

impl WorkProvider {
    pub fn open(
        repo_root: &Path,
        point: &RepositoryPoint,
        required: bool,
    ) -> Result<(Option<SharedWorkProvider>, Option<ActiveWork>)> {
        let config = RepoConfig::load_optional(repo_root)?
            .unwrap_or_else(|| RepoConfig::default_for(repo_root));
        let provider = config.composition.work.as_deref().unwrap_or("teko");
        let Some(definition) = discover(provider, PluginKind::Work)? else {
            if required || config.composition.require_work {
                bail!(
                    "Cannot launch bounded work.\n\nRequired capability:\n  work.v1\n\nConfigured provider:\n  {provider}\n\nFailure:\n  executable not found"
                );
            }
            return Ok((None, None));
        };
        let bounded = required || config.composition.require_work || definition.required;
        let attempted = (|| {
            let plugin =
                PluginSession::spawn(provider, &definition, repo_root, point.repository.clone())
                    .with_context(|| format!("failed to initialize work provider `{provider}`"))?;
            let mut provider = Self {
                plugin,
                repo_root: repo_root.to_path_buf(),
                work: None,
            };
            let active = provider.current(point)?;
            Ok((
                Some(std::sync::Arc::new(std::sync::Mutex::new(provider))),
                active,
            ))
        })();
        match attempted {
            Ok(value) => Ok(value),
            Err(error) if bounded => Err(error),
            Err(error) => {
                eprintln!("work provider `{provider}` unavailable: {error:#}");
                Ok((None, None))
            }
        }
    }

    pub fn current(&mut self, point: &RepositoryPoint) -> Result<Option<ActiveWork>> {
        let current: CurrentWorkResult = self.plugin.call(
            CURRENT_METHOD,
            CurrentWorkParams {
                repository_point: point.clone(),
                previously_known_work: None,
            },
        )?;
        match current {
            CurrentWorkResult::None => {
                self.work = None;
                Ok(None)
            }
            CurrentWorkResult::Active { work } => {
                let gate_authority = self.gate_authority(&work)?;
                let explained: ExplainResult = self.plugin.call(
                    EXPLAIN_METHOD,
                    AuthorityParams {
                        repository_point: point,
                        gate_authority: &gate_authority,
                    },
                )?;
                let status = explained
                    .status
                    .context("work provider returned active work without workflow status")?;
                self.work = Some((*work).clone());
                Ok(Some(ActiveWork {
                    packet: *work,
                    status,
                }))
            }
        }
    }

    pub fn observe_result(
        &mut self,
        point: &RepositoryPoint,
        result: WorkerRunResultEnvelope,
        outcome: ProcessOutcome,
    ) -> Result<String> {
        let binding = result
            .binding
            .as_ref()
            .context("cannot observe an unbound worker result")?;
        let related_evidence = result
            .evidence
            .iter()
            .map(|evidence| evidence.id.clone())
            .collect();
        let work = binding.work.clone();
        let _: serde_json::Value = self.plugin.call(
            OBSERVE_METHOD,
            ObserveParams {
                work,
                worker_result: result,
                repository_point: point.clone(),
                related_evidence,
                outcome,
            },
        )?;
        let gate_authority = self
            .work
            .as_ref()
            .map(|work| self.gate_authority(work))
            .transpose()?
            .unwrap_or_default();
        let explained: ExplainResult = self.plugin.call(
            EXPLAIN_METHOD,
            AuthorityParams {
                repository_point: point,
                gate_authority: &gate_authority,
            },
        )?;
        Ok(serde_json::to_string(&explained)?)
    }

    pub fn observe_gate_receipts(
        &mut self,
        point: &RepositoryPoint,
        work: &WorkPacketEnvelope,
        receipts: &[crate::gates::GateReceipt],
    ) -> Result<()> {
        let gate_authority = self.gate_authority(work)?;
        let _: serde_json::Value = self.plugin.call(
            OBSERVE_GATES_METHOD,
            ObserveGatesParams {
                work: work.work.id.clone(),
                repository_point: point,
                gate_authority: &gate_authority,
                receipts,
            },
        )?;
        Ok(())
    }

    pub fn close(
        &mut self,
        point: &RepositoryPoint,
        work: &WorkPacketEnvelope,
    ) -> Result<CloseResult> {
        let gate_authority = self.gate_authority(work)?;
        self.plugin.call(
            CLOSE_METHOD,
            AuthorityParams {
                repository_point: point,
                gate_authority: &gate_authority,
            },
        )
    }

    fn gate_authority(&self, work: &WorkPacketEnvelope) -> Result<GateAuthority> {
        if work.gates.is_empty() || !work.extensions.contains_key(REQUIRED_GATES_EXTENSION) {
            return Ok(GateAuthority::new());
        }
        let required = required_gate_ids(work)?;
        if required.is_empty() {
            return Ok(GateAuthority::new());
        }
        let config = RepoConfig::load_optional(&self.repo_root)?
            .unwrap_or_else(|| RepoConfig::default_for(&self.repo_root));
        Ok(GateCatalog::load(&self.repo_root, &config)?.authority(&required))
    }
}

pub(crate) fn required_gate_ids(packet: &WorkPacketEnvelope) -> Result<Vec<GateId>> {
    let extension = packet
        .extensions
        .get(REQUIRED_GATES_EXTENSION)
        .context("active work omitted the Toimija Gate ID extension")?
        .as_array()
        .context("active work Gate ID extension is not an array")?;
    let extension_ids = extension
        .iter()
        .map(|value| {
            GateId::parse(
                value
                    .as_str()
                    .context("active work Gate ID is not a string")?,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let wire_ids = packet
        .gates
        .iter()
        .map(|gate| {
            if !gate.command.is_empty() {
                bail!("active work supplied a gate command; only Gate IDs are accepted");
            }
            GateId::parse(
                gate.id
                    .as_str()
                    .strip_prefix("gate_")
                    .context("active work gate reference has the wrong prefix")?,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    if extension_ids != wire_ids {
        bail!("active work Gate ID extension disagrees with its work packet");
    }
    Ok(extension_ids)
}

pub fn has_active_work(repo_root: &Path) -> Result<bool> {
    Ok(active_work(repo_root)?.is_some())
}

pub fn active_work(repo_root: &Path) -> Result<Option<ActiveWork>> {
    let runner = ProcessCommandRunner;
    let context = crate::context::build_context_packet(&runner, repo_root, false)?;
    let point = crate::semantic::repository_point(&runner, &context)?;
    active_work_at(repo_root, &point, false)
}

pub fn active_work_at(
    repo_root: &Path,
    point: &RepositoryPoint,
    required: bool,
) -> Result<Option<ActiveWork>> {
    Ok(WorkProvider::open(repo_root, point, required)?.1)
}

pub fn explain_at(repo_root: &Path, point: &RepositoryPoint) -> Result<ExplainResult> {
    let config =
        RepoConfig::load_optional(repo_root)?.unwrap_or_else(|| RepoConfig::default_for(repo_root));
    let provider = config.composition.work.as_deref().unwrap_or("teko");
    let definition = discover(provider, PluginKind::Work)?
        .with_context(|| format!("work provider `{provider}` is unavailable"))?;
    let mut plugin =
        PluginSession::spawn(provider, &definition, repo_root, point.repository.clone())?;
    plugin.call(
        EXPLAIN_METHOD,
        ExplainParams {
            repository_point: point.clone(),
        },
    )
}

pub fn active_work_packet(repo_root: &Path) -> Result<Option<String>> {
    Ok(active_work(repo_root)?.map(|active| render(&active)))
}

pub(crate) fn render(active: &ActiveWork) -> String {
    let packet = &active.packet;
    let mut lines = vec![
        "# Active Teko Work".to_string(),
        String::new(),
        format!("Work: `{}`", packet.work.id),
        format!("Spec: `{}` — {}", packet.spec.id, packet.spec.title),
        format!("Objective: {}", packet.work.objective),
        "Obligations:".into(),
    ];
    lines.extend(packet.obligations.iter().map(|obligation| {
        let resolved = active
            .status
            .obligations
            .iter()
            .find(|state| state.obligation == obligation.id)
            .is_some_and(|state| state.supported);
        format!(
            "- [{}] `{}` {}",
            if resolved { "done" } else { "open" },
            obligation.id,
            obligation.description
        )
    }));
    if !packet.spec.non_goals.is_empty() {
        lines.push("Non-goals:".into());
        lines.extend(packet.spec.non_goals.iter().map(|item| format!("- {item}")));
    }
    lines.push("Write scope:".into());
    lines.extend(packet.scope.write.iter().map(|path| format!("- {path}")));
    lines.push("Closure blockers:".into());
    if active.status.blockers.is_empty() {
        lines.push("- none".into());
    } else {
        lines.extend(
            active
                .status
                .blockers
                .iter()
                .map(|blocker| format!("- `{}`: {}", blocker.code, blocker.message)),
        );
    }
    lines.join("\n")
}
