use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};
use false_agent_protocol::{
    RepositoryPoint,
    context::ExecutionContextEnvelope,
    plugin::PluginKind,
    worker::{
        DESCRIBE_METHOD, EnvironmentEntry, ExecutionMode, FINALIZE_METHOD, FinalizeParams,
        LaunchPlan, NormalizedWorkerResult, PREPARE_METHOD, PrepareParams, ResultContract,
        RuntimeArtifact, StdinMode, TerminalCapabilities, TerminalMode, WorkerDescriptor,
    },
};

use crate::plugins::{PluginDefinition, PluginSession, discover};

pub trait WorkerAdapter {
    fn describe(&mut self) -> Result<WorkerDescriptor>;
    fn prepare(
        &mut self,
        context: &ExecutionContextEnvelope,
        repository_root: &Path,
        session_directory: &Path,
        arguments: &[String],
        prompt: Option<String>,
    ) -> Result<LaunchPlan>;
    fn finalize(&mut self, input: FinalizeParams) -> Result<NormalizedWorkerResult>;
}

pub struct BuiltInAdapter {
    program: String,
    prompt_style: crate::adapters::agents::PromptStyle,
}

impl BuiltInAdapter {
    pub fn new(
        program: impl Into<String>,
        prompt_style: crate::adapters::agents::PromptStyle,
    ) -> Self {
        Self {
            program: program.into(),
            prompt_style,
        }
    }
}

impl WorkerAdapter for BuiltInAdapter {
    fn describe(&mut self) -> Result<WorkerDescriptor> {
        Ok(WorkerDescriptor {
            name: basename(&self.program).into(),
            version: "external-cli".into(),
            operating_systems: vec![std::env::consts::OS.into()],
            features: vec![false_agent_protocol::worker::WorkerFeature::InitialContext],
            context_modes: vec![false_agent_protocol::worker::ContextMode::File],
        })
    }

    fn prepare(
        &mut self,
        _context: &ExecutionContextEnvelope,
        repository_root: &Path,
        session_directory: &Path,
        arguments: &[String],
        prompt: Option<String>,
    ) -> Result<LaunchPlan> {
        let mut args = arguments.to_vec();
        if let Some(prompt) = prompt {
            match &self.prompt_style {
                crate::adapters::agents::PromptStyle::Positional => args.push(prompt),
                crate::adapters::agents::PromptStyle::Flag(flag) => {
                    args.push(flag.clone());
                    args.push(prompt);
                }
            }
        }
        Ok(LaunchPlan {
            program: self.program.clone(),
            args,
            env: vec![
                EnvironmentEntry {
                    key: "FALSE_SYSTEMS_CONTEXT_PATH".into(),
                    value: session_directory
                        .join(crate::semantic::CONTEXT_FILE)
                        .display()
                        .to_string(),
                },
                EnvironmentEntry {
                    key: "FALSE_SYSTEMS_RESULT_PATH".into(),
                    value: session_directory
                        .join(crate::semantic::RESULT_FILE)
                        .display()
                        .to_string(),
                },
            ],
            cwd: repository_root.display().to_string(),
            stdin_mode: StdinMode::Inherit,
            terminal_mode: TerminalMode::Inherit,
            context_artifacts: vec![RuntimeArtifact {
                path: session_directory
                    .join(crate::semantic::CONTEXT_FILE)
                    .display()
                    .to_string(),
                retention: false_agent_protocol::worker::RetentionPolicy::Ephemeral,
            }],
            result_contract: ResultContract {
                path: Some(
                    session_directory
                        .join(crate::semantic::RESULT_FILE)
                        .display()
                        .to_string(),
                ),
                structured: false,
            },
        })
    }

    fn finalize(&mut self, input: FinalizeParams) -> Result<NormalizedWorkerResult> {
        Ok(NormalizedWorkerResult {
            run: None,
            origin: input.origin.id,
            destination: input.destination.id,
            actions: Vec::new(),
            evidence: Vec::new(),
            claimed_completion: None,
            blockers: Vec::new(),
            result_grade: false_agent_protocol::worker::ResultGrade::ExitOnly,
        })
    }
}

pub struct ExternalPluginAdapter {
    session: PluginSession,
    definition: PluginDefinition,
}

impl WorkerAdapter for ExternalPluginAdapter {
    fn describe(&mut self) -> Result<WorkerDescriptor> {
        self.session.call(DESCRIBE_METHOD, serde_json::json!({}))
    }

    fn prepare(
        &mut self,
        context: &ExecutionContextEnvelope,
        repository_root: &Path,
        session_directory: &Path,
        arguments: &[String],
        _prompt: Option<String>,
    ) -> Result<LaunchPlan> {
        let plan = self.session.call(
            PREPARE_METHOD,
            PrepareParams {
                context: context.clone(),
                repository_root: repository_root.display().to_string(),
                session_directory: session_directory.display().to_string(),
                harness_arguments: arguments.to_vec(),
                terminal: TerminalCapabilities {
                    is_terminal: std::io::IsTerminal::is_terminal(&std::io::stdin()),
                    color: std::env::var_os("NO_COLOR").is_none(),
                    columns: None,
                    rows: None,
                },
                mode: if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                    ExecutionMode::Interactive
                } else {
                    ExecutionMode::NonInteractive
                },
            },
        )?;
        validate_launch_plan(&plan, repository_root, session_directory, &self.definition)?;
        Ok(plan)
    }

    fn finalize(&mut self, input: FinalizeParams) -> Result<NormalizedWorkerResult> {
        self.session.call(FINALIZE_METHOD, input)
    }
}

pub enum Adapter {
    BuiltIn(BuiltInAdapter),
    External(Box<ExternalPluginAdapter>),
}

impl WorkerAdapter for Adapter {
    fn describe(&mut self) -> Result<WorkerDescriptor> {
        match self {
            Self::BuiltIn(adapter) => adapter.describe(),
            Self::External(adapter) => adapter.describe(),
        }
    }

    fn prepare(
        &mut self,
        context: &ExecutionContextEnvelope,
        repository_root: &Path,
        session_directory: &Path,
        arguments: &[String],
        prompt: Option<String>,
    ) -> Result<LaunchPlan> {
        match self {
            Self::BuiltIn(adapter) => adapter.prepare(
                context,
                repository_root,
                session_directory,
                arguments,
                prompt,
            ),
            Self::External(adapter) => adapter.prepare(
                context,
                repository_root,
                session_directory,
                arguments,
                prompt,
            ),
        }
    }

    fn finalize(&mut self, input: FinalizeParams) -> Result<NormalizedWorkerResult> {
        match self {
            Self::BuiltIn(adapter) => adapter.finalize(input),
            Self::External(adapter) => adapter.finalize(input),
        }
    }
}

pub fn select(
    program: &str,
    repo_root: &Path,
    repository: &RepositoryPoint,
    prompt_style: crate::adapters::agents::PromptStyle,
) -> Result<Adapter> {
    let id = basename(program);
    if let Some(definition) = discover(id, PluginKind::Worker)? {
        let session =
            PluginSession::spawn(id, &definition, repo_root, repository.repository.clone())?;
        return Ok(Adapter::External(Box::new(ExternalPluginAdapter {
            session,
            definition,
        })));
    }
    Ok(Adapter::BuiltIn(BuiltInAdapter::new(program, prompt_style)))
}

pub fn validate_launch_plan(
    plan: &LaunchPlan,
    repository_root: &Path,
    session_directory: &Path,
    definition: &PluginDefinition,
) -> Result<()> {
    if basename(&plan.program) != basename(&definition.command) {
        bail!("launch plan executable is not the configured plugin executable");
    }
    if matches!(basename(&plan.program), "sh" | "bash" | "zsh")
        && plan.args.first().is_some_and(|arg| arg == "-c")
    {
        bail!("launch plan may not invoke a shell command string");
    }
    let cwd = Path::new(&plan.cwd);
    if normalize(cwd) != normalize(repository_root) {
        bail!("launch plan cwd is not the verified repository root");
    }
    if plan.args.len() > false_agent_protocol::plugin::MAX_ARGUMENTS
        || plan
            .args
            .iter()
            .any(|arg| arg.len() > false_agent_protocol::plugin::MAX_ARGUMENT_LENGTH)
    {
        bail!("launch plan arguments exceed protocol limits");
    }
    if plan.env.len() > false_agent_protocol::plugin::MAX_ENVIRONMENT_ENTRIES {
        bail!("launch plan environment exceeds protocol limits");
    }
    for entry in &plan.env {
        if !matches!(
            entry.key.as_str(),
            "FALSE_SYSTEMS_CONTEXT_PATH" | "FALSE_SYSTEMS_RESULT_PATH"
        ) {
            bail!("launch plan environment key `{}` is not allowed", entry.key);
        }
    }
    for path in plan
        .context_artifacts
        .iter()
        .map(|artifact| artifact.path.as_str())
        .chain(plan.result_contract.path.as_deref())
    {
        if !contained(Path::new(path), session_directory) {
            bail!("launch plan runtime path escapes the session directory");
        }
    }
    Ok(())
}

fn contained(path: &Path, root: &Path) -> bool {
    path.is_absolute()
        && normalize(path).starts_with(normalize(root))
        && !path
            .components()
            .any(|component| component == Component::ParentDir)
}

fn normalize(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn basename(program: &str) -> &str {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_plan_rejects_shell_and_escaped_paths() {
        let repo = Path::new("/repo");
        let session = Path::new("/repo/.toimija/sessions/1");
        let definition = PluginDefinition {
            kind: PluginKind::Worker,
            command: "sh".into(),
            args: Vec::new(),
            required: false,
            timeout_ms: 5_000,
        };
        let mut plan = LaunchPlan {
            program: "sh".into(),
            args: vec!["-c".into(), "echo no".into()],
            env: Vec::new(),
            cwd: repo.display().to_string(),
            stdin_mode: StdinMode::Inherit,
            terminal_mode: TerminalMode::Inherit,
            context_artifacts: Vec::new(),
            result_contract: ResultContract {
                path: None,
                structured: false,
            },
        };
        assert!(validate_launch_plan(&plan, repo, session, &definition).is_err());
        plan.program = "worker".into();
        plan.args.clear();
        plan.context_artifacts.push(RuntimeArtifact {
            path: "/repo/.toimija/sessions/1/../escape".into(),
            retention: false_agent_protocol::worker::RetentionPolicy::Ephemeral,
        });
        let definition = PluginDefinition {
            command: "worker".into(),
            ..definition
        };
        assert!(validate_launch_plan(&plan, repo, session, &definition).is_err());
    }
}
