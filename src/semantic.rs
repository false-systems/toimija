use std::path::Path;

use anyhow::{Context, Result};
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

/// The session's current repository point, as a `RepositoryPointEnvelope`.
/// A wrapped command finds it through `FALSE_SYSTEMS_REPOSITORY_POINT_PATH`.
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

/// Write `value` as JSON to `path`, a file under `.toimija/` whose directory
/// already exists; never through a symlink.
pub fn write_json(repo_root: &Path, path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    crate::owned::replace(repo_root, path, &bytes)
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
}
