use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use false_agent_protocol::{
    EvidenceRef, RepositoryPointRef, SpecRef, WorkRef, WorkerRunRef, WorkerRunResultEnvelope,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::command::CommandRunner;
use crate::sauma::{self, SaumaEnvelope, SaumaProvenance};

pub const SCHEMA: &str = "work-correlation.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceGrade {
    Declared,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullRequestReference {
    pub provider: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
    pub r#ref: String,
    pub grade: ReferenceGrade,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkCorrelation {
    pub schema: String,
    pub created_at: String,
    pub spec: SpecRef,
    pub work: WorkRef,
    pub session: String,
    pub worker_run: WorkerRunRef,
    pub origin: RepositoryPointRef,
    pub destination: RepositoryPointRef,
    pub commit: String,
    pub pull_request: PullRequestReference,
    pub evidence: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sauma: Option<SaumaEnvelope>,
}

impl sauma::Carrier for WorkCorrelation {
    fn sauma_mut(&mut self) -> &mut Option<SaumaEnvelope> {
        &mut self.sauma
    }
}

pub struct CorrelateInput {
    pub url: String,
    pub session: Option<String>,
    pub run: Option<String>,
}

pub fn create(
    runner: &impl CommandRunner,
    repo_root: &Path,
    input: CorrelateInput,
) -> Result<(WorkCorrelation, PathBuf)> {
    let pull_request = parse_pull_request_url(&input.url)?;
    ensure_origin_matches(runner, repo_root, &pull_request.repository)?;

    let result = read_result(repo_root, input.run.as_deref())?;
    let session = session_for_run(repo_root, &result, input.session.as_deref())?;
    let binding = result
        .binding
        .as_ref()
        .context("latest worker result is unbound; only bounded Teko work can correlate to a PR")?;
    let destination = result
        .destination
        .clone()
        .context("latest worker result has no authoritative destination")?;
    let evidence_digests = result
        .evidence
        .iter()
        .map(|evidence| evidence.digest.clone())
        .collect();

    let packet = crate::context::build_context_packet(runner, repo_root, false)?;
    ensure!(
        !packet.git.dirty,
        "repository is dirty; correlate the PR only from its committed destination"
    );
    let point = crate::semantic::repository_point(runner, &packet)?;
    ensure!(
        point.id == destination,
        "current repository point {} does not match worker destination {}",
        point.id,
        destination
    );
    let commit = packet
        .git
        .head_oid
        .context("repository has no HEAD commit")?;
    let commit_tree = git_stdout(runner, repo_root, &["rev-parse", "HEAD^{tree}"])?;
    ensure!(
        commit_tree == point.tree_oid.0,
        "HEAD tree {commit_tree} does not match destination tree {}",
        point.tree_oid.0
    );

    let mut correlation = WorkCorrelation {
        schema: SCHEMA.into(),
        created_at: now_timestamp(),
        spec: binding.spec.clone(),
        work: binding.work.clone(),
        session,
        worker_run: result.run.clone(),
        origin: result.origin.clone(),
        destination,
        commit: commit.clone(),
        pull_request,
        evidence: result
            .evidence
            .iter()
            .map(|evidence| evidence.id.clone())
            .collect(),
        sauma: None,
    };
    correlation.attach(&point.consistency_token.0, commit, evidence_digests)?;

    let path = correlation_path(repo_root, &result.run);
    if path.exists() {
        let existing = read_path(&path)?;
        ensure!(
            existing.spec == correlation.spec
                && existing.work == correlation.work
                && existing.session == correlation.session
                && existing.worker_run == correlation.worker_run
                && existing.origin == correlation.origin
                && existing.destination == correlation.destination
                && existing.commit == correlation.commit
                && existing.pull_request == correlation.pull_request
                && existing.evidence == correlation.evidence,
            "worker run {} is already correlated to {}",
            result.run,
            existing.pull_request.r#ref
        );
        return Ok((existing, path));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut bytes = serde_json::to_vec_pretty(&correlation)?;
    bytes.push(b'\n');
    crate::atomic_write::replace(&path, &bytes)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok((correlation, path))
}

impl WorkCorrelation {
    fn attach(
        &mut self,
        anchored_at: &str,
        commit: String,
        evidence_digests: Vec<String>,
    ) -> Result<()> {
        let mut provenance = vec![
            SaumaProvenance::Spec {
                id: self.spec.to_string(),
            },
            SaumaProvenance::Work {
                id: self.work.to_string(),
            },
            SaumaProvenance::Session {
                id: self.session.clone(),
            },
            SaumaProvenance::WorkerRun {
                id: self.worker_run.to_string(),
            },
            SaumaProvenance::RepositoryPoint {
                id: self.origin.to_string(),
            },
            SaumaProvenance::RepositoryPoint {
                id: self.destination.to_string(),
            },
            SaumaProvenance::Git { oid: commit },
            SaumaProvenance::External {
                r#ref: self.pull_request.r#ref.clone(),
            },
        ];
        provenance.extend(
            evidence_digests
                .into_iter()
                .map(|digest| SaumaProvenance::Artifact { digest }),
        );
        sauma::attach(
            self,
            SCHEMA,
            provenance,
            anchored_at,
            Some(self.session.clone()),
        )
        .map_err(anyhow::Error::from)
    }
}

pub fn latest(repo_root: &Path) -> Result<Option<WorkCorrelation>> {
    let dir = repo_root.join(".toimija/runs");
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", dir.display()));
        }
    };
    let mut paths = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".correlation.json"))
        })
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect::<Vec<_>>();
    paths.sort_by(|left, right| right.0.cmp(&left.0));
    paths.first().map(|(_, path)| read_path(path)).transpose()
}

fn read_result(repo_root: &Path, run: Option<&str>) -> Result<WorkerRunResultEnvelope> {
    match run {
        Some(run) => {
            let run: WorkerRunRef = run.parse().map_err(anyhow::Error::from)?;
            crate::semantic::read_result(
                &repo_root
                    .join(".toimija/runs")
                    .join(format!("{run}.result.json")),
            )
        }
        None => crate::semantic::latest_result(repo_root)?
            .context("no durable worker result; run bounded work through Toimija first"),
    }
}

fn correlation_path(repo_root: &Path, run: &WorkerRunRef) -> PathBuf {
    repo_root
        .join(".toimija/runs")
        .join(format!("{run}.correlation.json"))
}

fn read_path(path: &Path) -> Result<WorkCorrelation> {
    const MAX_BYTES: u64 = 1024 * 1024;
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= MAX_BYTES,
        "correlation artifact must be a bounded regular file"
    );
    let mut correlation: WorkCorrelation = serde_json::from_slice(&fs::read(path)?)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    ensure!(
        correlation.schema == SCHEMA,
        "unsupported correlation schema"
    );
    ensure!(
        correlation
            .sauma
            .as_ref()
            .is_some_and(|envelope| envelope.envelope == sauma::ENVELOPE_SCHEMA),
        "correlation artifact has no supported Sauma envelope"
    );
    ensure!(
        sauma::verify(&mut correlation)?.is_none(),
        "correlation artifact digest mismatch"
    );
    Ok(correlation)
}

fn session_for_run(
    repo_root: &Path,
    result: &WorkerRunResultEnvelope,
    expected: Option<&str>,
) -> Result<String> {
    let run = &result.run;
    let receipt = crate::session::read_run_receipt(repo_root, run)?;
    ensure!(
        receipt.worker_run == *run,
        "run receipt references worker run {}, expected {run}",
        receipt.worker_run
    );
    ensure!(
        receipt.result_digest == result.result_digest,
        "run receipt does not match worker result {run}"
    );
    if let Some(id) = expected {
        ensure!(
            receipt.session == id,
            "worker run {run} was produced by session {}, not {id}",
            receipt.session
        );
    }
    ensure!(
        crate::session::is_valid_session_id(&receipt.session),
        "run receipt has invalid session id `{}`",
        receipt.session
    );
    validate_worktree(repo_root, &receipt.session, &receipt.worktree)?;

    let session_dir = repo_root
        .join(crate::session::SESSIONS_SUBDIR)
        .join(&receipt.session);
    match fs::symlink_metadata(session_dir.join("session.json")) {
        Ok(_) => {
            validate_session(repo_root, &receipt.session)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("failed to inspect producing session marker"),
    }
    Ok(receipt.session)
}

fn validate_session(repo_root: &Path, id: &str) -> Result<crate::session::SessionMarker> {
    ensure!(
        crate::session::is_valid_session_id(id),
        "invalid session id `{id}`"
    );
    let marker =
        crate::session::load_marker(&repo_root.join(crate::session::SESSIONS_SUBDIR).join(id))?;
    validate_marker(repo_root, id, &marker)?;
    Ok(marker)
}

fn validate_marker(
    repo_root: &Path,
    id: &str,
    marker: &crate::session::SessionMarker,
) -> Result<()> {
    ensure!(marker.id == id, "session marker id does not match `{id}`");
    ensure!(
        marker.status == crate::session::SessionStatus::Closed,
        "session {id} is still active"
    );
    validate_worktree(repo_root, id, &marker.worktree)
}

fn validate_worktree(repo_root: &Path, id: &str, worktree: &str) -> Result<()> {
    let expected = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.into());
    let observed = Path::new(worktree)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(worktree));
    ensure!(
        observed == expected,
        "session {id} belongs to a different worktree"
    );
    Ok(())
}

fn ensure_origin_matches(
    runner: &impl CommandRunner,
    repo_root: &Path,
    expected: &str,
) -> Result<()> {
    let remote = git_stdout(runner, repo_root, &["remote", "get-url", "origin"])?;
    let observed = repository_slug(&remote).context("origin is not a supported GitHub remote")?;
    ensure!(
        observed.eq_ignore_ascii_case(expected),
        "PR repository {expected} does not match origin {observed}"
    );
    Ok(())
}

fn git_stdout(runner: &impl CommandRunner, repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = runner.run(repo_root, "git", args)?;
    ensure!(
        output.status == 0,
        "git {} failed with status {}",
        args.join(" "),
        output.status
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn parse_pull_request_url(url: &str) -> Result<PullRequestReference> {
    let rest = url
        .strip_prefix("https://github.com/")
        .context("PR URL must start with https://github.com/")?;
    let parts = rest.trim_end_matches('/').split('/').collect::<Vec<_>>();
    ensure!(
        parts.len() == 4 && parts[2] == "pull",
        "PR URL must have the form https://github.com/<owner>/<repo>/pull/<number>"
    );
    ensure!(
        valid_slug_part(parts[0]) && valid_slug_part(parts[1]),
        "PR URL contains an invalid repository name"
    );
    let number = parts[3].parse::<u64>().context("invalid PR number")?;
    ensure!(number > 0, "PR number must be nonzero");
    let repository = format!("{}/{}", parts[0], parts[1]);
    Ok(PullRequestReference {
        provider: "github".into(),
        repository: repository.clone(),
        number,
        url: format!("https://github.com/{repository}/pull/{number}"),
        r#ref: format!("github:{repository}#{number}"),
        grade: ReferenceGrade::Declared,
    })
}

fn repository_slug(remote: &str) -> Option<String> {
    let path = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("https://github.com/"))?
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    (parts.next().is_none() && valid_slug_part(owner) && valid_slug_part(repo))
        .then(|| format!("{owner}/{repo}"))
}

fn valid_slug_part(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn now_timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_github_pr_urls_and_remotes() {
        let pr = parse_pull_request_url("https://github.com/false-systems/kisko/pull/171").unwrap();
        assert_eq!(pr.repository, "false-systems/kisko");
        assert_eq!(pr.r#ref, "github:false-systems/kisko#171");
        assert_eq!(
            repository_slug("git@github.com:false-systems/kisko.git").as_deref(),
            Some("false-systems/kisko")
        );
        assert_eq!(
            repository_slug("https://github.com/false-systems/kisko.git").as_deref(),
            Some("false-systems/kisko")
        );
    }

    #[test]
    fn rejects_ambiguous_pr_urls() {
        assert!(parse_pull_request_url("https://example.com/a/b/pull/1").is_err());
        assert!(parse_pull_request_url("https://github.com/a/b/issues/1").is_err());
        assert!(parse_pull_request_url("https://github.com/a/b/pull/0").is_err());
    }
}
