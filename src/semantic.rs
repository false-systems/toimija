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
use crate::consistency::{CONSISTENCY_ALGORITHM, Consistency};
use crate::context::ContextPacket;
use crate::git::{GitState, Worktree};

/// The session's current repository point, as a `RepositoryPointEnvelope`.
/// A wrapped command finds it through `FALSE_SYSTEMS_REPOSITORY_POINT_PATH`.
pub const POINT_FILE: &str = "semantic-point.json";

/// The extension naming the algorithm behind an envelope's `tree_oid`. The
/// protocol's `RepositoryPoint` has no field for it, and its envelopes accept
/// additive extensions. An envelope without it was written before Toimija named
/// its tree algorithm, and its tree is the legacy one
/// ([`crate::snapshot::LEGACY_TREE_ALGORITHM`]).
pub const TREE_ALGORITHM_EXTENSION: &str = "tree_algorithm";

/// The extension naming the version of an envelope's consistency token, which
/// the protocol carries as a bare string. The token itself stays
/// `sha256:<hex>`, the shape sauma's envelope schema requires of an anchor; its
/// hash covers the version name. An envelope without this extension was
/// written by an earlier toimija, and its token is of an earlier version.
pub const CONSISTENCY_ALGORITHM_EXTENSION: &str = "consistency_algorithm";

/// How every point id Toimija writes begins. The id is derived from the tree
/// as well as the token ([`point_ref`]), and this marker says so. An id that
/// is `rpoint_` and 64 hex digits, without it, was derived from the repository
/// and the token alone, before KATANA D9: it keeps that meaning, and is never
/// compared with one of these.
pub const POINT_ID_PREFIX: &str = "rpoint_v3_";

/// How many times a point is attempted, each from a fresh read of the
/// repository, while the worktree keeps moving under the tree write. Then it is
/// refused rather than paired across a move the token observed.
pub const POINT_ATTEMPTS: usize = 3;

/// The pause before each retry: the worktree was moving a moment ago, so give
/// it a moment to settle. Short and growing; 150 ms in all at most.
pub const POINT_BACKOFF: [std::time::Duration; POINT_ATTEMPTS - 1] = [
    std::time::Duration::from_millis(50),
    std::time::Duration::from_millis(100),
];

/// Run `attempt` again while it fails only because the worktree moved under
/// the tree write ([`crate::snapshot::WorktreeMoved`]), up to
/// [`POINT_ATTEMPTS`] times in all, pausing [`POINT_BACKOFF`] before each
/// retry. Each attempt must read the repository afresh: retrying with the
/// token it already holds would only fail again. Any other error, and the last
/// move, is returned as it is.
pub fn retry_moved<T>(attempt: impl FnMut() -> Result<T>) -> Result<T> {
    retry_moved_with(std::thread::sleep, attempt)
}

/// [`retry_moved`], with the pause given (tests observe it instead of waiting).
fn retry_moved_with<T>(
    mut pause: impl FnMut(std::time::Duration),
    mut attempt: impl FnMut() -> Result<T>,
) -> Result<T> {
    let mut attempts = 1;
    loop {
        match attempt() {
            Err(error)
                if attempts < POINT_ATTEMPTS && crate::snapshot::is_worktree_moved(&error) =>
            {
                pause(POINT_BACKOFF[attempts - 1]);
                attempts += 1;
            }
            Err(error) if crate::snapshot::is_worktree_moved(&error) => {
                return Err(error.context(format!(
                    "the worktree kept moving across {POINT_ATTEMPTS} attempts; no repository \
                     point was written"
                )));
            }
            other => return other,
        }
    }
}

/// What an envelope carrying a point names beside it: the algorithm of its
/// tree and the version of its token.
fn point_extensions() -> Extensions {
    let mut extensions = Extensions::new();
    extensions.insert(
        TREE_ALGORITHM_EXTENSION.to_string(),
        serde_json::Value::String(crate::snapshot::TREE_ALGORITHM.id().to_string()),
    );
    extensions.insert(
        CONSISTENCY_ALGORITHM_EXTENSION.to_string(),
        serde_json::Value::String(CONSISTENCY_ALGORITHM.to_string()),
    );
    extensions
}

pub fn execution_context(
    runner: &impl CommandRunner,
    worktree: &Worktree,
    context: &ContextPacket,
    generation: u64,
) -> Result<ExecutionContextEnvelope> {
    let repository = repository_point(runner, worktree, &context.git, &context.consistency)?;
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
        extensions: point_extensions(),
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

/// The repository point of the state `git` was read as, whose token is
/// `consistency`: the token, and the tree-identity.v1 tree written from the
/// worktree. A point reads nothing else: `git` may be a whole packet's state
/// or only what a token fingerprints. Its id is derived from both
/// ([`point_ref`]), so two points with one id hold one tree, whatever the
/// token does not observe. The token is read again after the tree is written,
/// so the two describe one state within what the token observes; if the
/// worktree moved in between, this fails with
/// [`crate::snapshot::WorktreeMoved`] and the caller reads the state again and
/// tries again ([`retry_moved`]).
pub fn repository_point(
    runner: &impl CommandRunner,
    worktree: &Worktree,
    git: &GitState,
    consistency: &Consistency,
) -> Result<RepositoryPoint> {
    let tree = crate::snapshot::tree_at_token(runner, worktree, git, &consistency.token)
        .context("failed to materialize the authoritative repository point")?;
    let repository = repository_ref(&git.repo_root)?;
    let id = point_ref(
        &repository,
        &consistency.algorithm,
        &consistency.token,
        crate::snapshot::TREE_ALGORITHM.id(),
        &tree,
    )?;
    Ok(RepositoryPoint {
        id,
        repository,
        consistency_token: ConsistencyToken(consistency.token.clone()),
        tree_oid: TreeOid(tree),
        branch: git.branch.clone().map(BranchName),
        dirty: git.dirty,
        captured_at: now_timestamp(),
    })
}

/// Wrap a point from [`repository_point`], naming the algorithm of its tree.
pub fn point_envelope(point: RepositoryPoint) -> RepositoryPointEnvelope {
    RepositoryPointEnvelope {
        protocol: ProtocolVersion::V1,
        repository: point,
        extensions: point_extensions(),
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

/// A point's id: [`POINT_ID_PREFIX`], then a SHA-256 over everything that
/// makes it this point — the repository, the consistency token and its
/// algorithm, the tree and its algorithm — each field ended by a NUL, which
/// none of them can contain. Equal ids therefore mean equal trees by
/// construction, and ids of trees under different algorithms never meet.
fn point_ref(
    repository: &RepositoryRef,
    consistency_algorithm: &str,
    consistency_token: &str,
    tree_algorithm: &str,
    tree: &str,
) -> Result<RepositoryPointRef> {
    let mut digest = Sha256::new();
    for field in [
        POINT_ID_PREFIX,
        repository.as_str(),
        consistency_algorithm,
        consistency_token,
        tree_algorithm,
        tree,
    ] {
        digest.update(field.as_bytes());
        digest.update([0]);
    }
    RepositoryPointRef::parse(format!("{POINT_ID_PREFIX}{:x}", digest.finalize()))
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
    use crate::snapshot::WorktreeMoved;

    #[test]
    fn a_moving_worktree_is_retried_a_bounded_number_of_times_then_refused() {
        // Moves once, then holds still: the second attempt's point stands.
        let mut calls = 0;
        let settled = retry_moved(|| {
            calls += 1;
            if calls == 1 {
                Err(anyhow::Error::from(WorktreeMoved).context("materializing"))
            } else {
                Ok(calls)
            }
        });
        assert_eq!(settled.unwrap(), 2);

        // Never holds still: refused after the last attempt, saying so.
        let mut calls = 0;
        let refused: Result<()> = retry_moved(|| {
            calls += 1;
            Err(WorktreeMoved.into())
        });
        assert_eq!(calls, POINT_ATTEMPTS);
        let refused = refused.unwrap_err();
        assert!(crate::snapshot::is_worktree_moved(&refused));
        assert!(
            format!("{refused:#}").contains("kept moving"),
            "{refused:#}"
        );

        // Any other failure is not a reason to try again.
        let mut calls = 0;
        let failed: Result<()> = retry_moved(|| {
            calls += 1;
            Err(anyhow::anyhow!("git is broken"))
        });
        assert_eq!(calls, 1);
        assert_eq!(failed.unwrap_err().to_string(), "git is broken");
    }

    /// Between attempts the worktree gets a moment to settle: a short pause
    /// that grows, bounded in count and in total, and none when there is
    /// nothing to retry.
    #[test]
    fn a_retry_waits_a_short_bounded_while_before_reading_again() {
        let mut pauses = Vec::new();
        let _: Result<()> =
            retry_moved_with(|pause| pauses.push(pause), || Err(WorktreeMoved.into()));
        assert_eq!(pauses, POINT_BACKOFF.to_vec());
        assert_eq!(pauses.len(), POINT_ATTEMPTS - 1);
        assert!(
            pauses.windows(2).all(|pair| pair[0] < pair[1]),
            "{pauses:?}"
        );
        let total: std::time::Duration = pauses.iter().sum();
        assert!(total <= std::time::Duration::from_millis(250), "{total:?}");

        let mut pauses = Vec::new();
        let _ = retry_moved_with(|pause| pauses.push(pause), || Ok(()));
        let _: Result<()> = retry_moved_with(
            |pause| pauses.push(pause),
            || Err(anyhow::anyhow!("git is broken")),
        );
        assert!(pauses.is_empty(), "{pauses:?}");
    }

    #[test]
    fn a_point_envelope_names_its_tree_algorithm_and_token_version() {
        let extensions = point_extensions();
        assert_eq!(
            extensions[TREE_ALGORITHM_EXTENSION],
            serde_json::Value::String("tree-identity.v1".to_string())
        );
        assert_eq!(
            extensions[CONSISTENCY_ALGORITHM_EXTENSION],
            serde_json::Value::String("toimija-consistency-v3".to_string())
        );
    }

    #[test]
    fn repository_and_point_refs_are_typed_and_deterministic() {
        let repository = repository_ref(Path::new(".")).unwrap();
        assert!(repository.as_str().starts_with("repo_"));
        let id = |token: &str, tree: &str| {
            point_ref(&repository, "consistency", token, "tree-identity.v1", tree).unwrap()
        };
        assert_eq!(id("token", "tree"), id("token", "tree"));
        assert_ne!(id("token", "tree"), id("other", "tree"));
        assert!(id("token", "tree").as_str().starts_with(POINT_ID_PREFIX));
    }

    /// The id is a function of the tree: one token with two trees — every
    /// case in issue #239 — is two points, and so is one tree under two
    /// algorithms, or one token under two token algorithms.
    #[test]
    fn a_point_id_covers_its_tree_and_both_algorithms() {
        let repository = repository_ref(Path::new(".")).unwrap();
        let base = point_ref(&repository, "c", "token", "tree-identity.v1", "aaaa").unwrap();
        for other in [
            point_ref(&repository, "c", "token", "tree-identity.v1", "bbbb"),
            point_ref(&repository, "c", "token", "toimija-legacy", "aaaa"),
            point_ref(&repository, "c2", "token", "tree-identity.v1", "aaaa"),
            point_ref(
                &RepositoryRef::parse("repo_other").unwrap(),
                "c",
                "token",
                "tree-identity.v1",
                "aaaa",
            ),
        ] {
            assert_ne!(base, other.unwrap());
        }
        // Fields are delimited: moving a byte across a boundary is another id.
        assert_ne!(
            point_ref(&repository, "c", "token", "tree-identity.v1a", "aaa").unwrap(),
            base
        );
        // 64 hex digits after the marker, inside the protocol's id limits.
        let hex = base.as_str().strip_prefix(POINT_ID_PREFIX).unwrap();
        assert_eq!(hex.len(), 64);
        assert!(hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
