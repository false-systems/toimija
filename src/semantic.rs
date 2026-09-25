use std::path::Path;

use anyhow::{Context, Result, bail};
use false_agent_protocol::{
    BranchName, ChangedPath, CompletionLevel, ConsistencyToken, Extensions, ProtocolVersion,
    RepositoryPoint, RepositoryPointEnvelope, RepositoryPointRef, RepositoryRef, Timestamp,
    TreeOid,
    context::{ExecutionContextEnvelope, RepositoryCapabilities},
};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use tracing::info;

use crate::command::CommandRunner;
use crate::context::ContextPacket;

pub const CONTEXT_FILE: &str = "semantic-context.json";
pub const RESULT_FILE: &str = "semantic-result.json";
pub const POINT_FILE: &str = "semantic-point.json";

pub fn execution_context(
    runner: &impl CommandRunner,
    context: &ContextPacket,
    generation: u64,
) -> Result<ExecutionContextEnvelope> {
    let repository = repository_point(runner, context)?;
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
            reachable_completion: vec![CompletionLevel::Changed],
        },
        work: None,
        workflow_status: None,
        context_digest: String::new(),
        extensions: Extensions::new(),
    };
    envelope.seal().map_err(anyhow::Error::from)?;
    info!(
        event = "toimija_context_generated",
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
        bail!("refusing worker result through symlink {}", path.display());
    }
    if metadata.len() > MAX_BYTES {
        bail!("worker result exceeds the {MAX_BYTES}-byte limit");
    }
    let result = serde_json::from_slice::<false_agent_protocol::WorkerRunResultEnvelope>(
        &std::fs::read(path)?,
    )
    .context("worker result is not a valid worker-result envelope")?;
    result.validate().map_err(anyhow::Error::from)?;
    Ok(result)
}

#[derive(Debug)]
pub struct Correlation {
    pub result: false_agent_protocol::WorkerRunResultEnvelope,
    pub durable_path: std::path::PathBuf,
}

/// Keep the worker's structured result, if it left one. Toimija launched no
/// bound work, so a result that claims a work binding is refused.
pub fn correlate_result(repo_root: &Path, result_path: &Path) -> Result<Option<Correlation>> {
    if !result_path.is_file() {
        return Ok(None);
    }
    let result = read_result(result_path)?;
    if result.binding.is_some() {
        bail!(
            "Worker result {} claims bound work, but Toimija launches no bound work.",
            result.run
        );
    }

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
    }))
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
    fn a_result_that_claims_bound_work_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".toimija")).unwrap();
        let result: false_agent_protocol::WorkerRunResultEnvelope =
            serde_json::from_str(false_agent_protocol::WORKER_RESULT_FIXTURE).unwrap();
        assert!(result.binding.is_some());
        let result_path = dir.path().join(".toimija/result.json");
        write_json(&result_path, &result).unwrap();
        let error = correlate_result(dir.path(), &result_path)
            .unwrap_err()
            .to_string();
        assert!(error.contains("claims bound work"), "{error}");
    }
}
