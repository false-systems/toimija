//! Trusted repository gate definitions.
//!
//! Gate requirements carry only [`GateId`]s. This module is the only place
//! those IDs become executable plus argv, and it never parses a shell string.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use false_agent_protocol::GateReceiptRef;
use false_agent_protocol::{ConsistencyToken, RepositoryPointRef, RepositoryRef, TreeOid};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::command::CommandRunner;

const DEFAULT_TIMEOUT_SECS: u64 = 900;
const DEFAULT_OUTPUT_LIMIT: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GateId(String);

impl GateId {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 64
            || !value.bytes().enumerate().all(|(index, byte)| match byte {
                b'a'..=b'z' | b'0'..=b'9' => true,
                b'-' | b'_' => index > 0,
                _ => false,
            })
        {
            bail!("invalid gate ID `{value}`; use 1-64 lowercase letters, digits, `-`, or `_`");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GateId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for GateId {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

impl TryFrom<String> for GateId {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(value)
    }
}

impl From<GateId> for String {
    fn from(value: GateId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executable(OsString);

impl Executable {
    fn new(value: &str) -> Result<Self> {
        if value.trim().is_empty() {
            bail!("gate executable must not be empty");
        }
        let basename = Path::new(value)
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or(value);
        if matches!(
            basename,
            "sh" | "bash" | "zsh" | "fish" | "cmd" | "powershell" | "pwsh"
        ) {
            bail!("gate executable `{value}` is a shell; gates require a direct executable");
        }
        Ok(Self(value.into()))
    }

    pub fn as_os_str(&self) -> &OsStr {
        &self.0
    }
}

impl fmt::Display for Executable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.to_string_lossy())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argument(OsString);

impl Argument {
    fn new(value: &str) -> Self {
        Self(value.into())
    }

    pub fn as_os_str(&self) -> &OsStr {
        &self.0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateWorkingDirectoryConfig {
    #[default]
    RepositoryRoot,
    Relative(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateWorkingDirectory {
    RepositoryRoot,
    Relative(PathBuf),
}

impl GateWorkingDirectory {
    fn from_config(value: &GateWorkingDirectoryConfig) -> Result<Self> {
        match value {
            GateWorkingDirectoryConfig::RepositoryRoot => Ok(Self::RepositoryRoot),
            GateWorkingDirectoryConfig::Relative(path) => {
                if path.as_os_str().is_empty()
                    || path.is_absolute()
                    || path.components().any(|component| {
                        matches!(
                            component,
                            Component::ParentDir | Component::RootDir | Component::Prefix(_)
                        )
                    })
                {
                    bail!("gate cwd must be a non-empty repository-relative path without `..`");
                }
                Ok(Self::Relative(path.clone()))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnvironmentKey(String);

impl EnvironmentKey {
    fn parse(value: &str) -> Result<Self> {
        if value.is_empty() || value.contains('=') || value.bytes().any(|byte| byte == 0) {
            bail!("invalid gate environment key `{value}`");
        }
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentPolicy {
    pub inherit: Vec<EnvironmentKey>,
    pub set: BTreeMap<EnvironmentKey, String>,
    pub remove: Vec<EnvironmentKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvironmentPolicyConfig {
    pub inherit: Vec<String>,
    pub set: BTreeMap<String, String>,
    pub remove: Vec<String>,
}

impl Default for EnvironmentPolicyConfig {
    fn default() -> Self {
        Self {
            inherit: vec![
                "PATH".into(),
                "HOME".into(),
                "CARGO_HOME".into(),
                "RUSTUP_HOME".into(),
                "TMPDIR".into(),
            ],
            set: BTreeMap::new(),
            remove: Vec::new(),
        }
    }
}

impl EnvironmentPolicy {
    fn from_config(config: &EnvironmentPolicyConfig) -> Result<Self> {
        let inherit = config
            .inherit
            .iter()
            .map(|key| EnvironmentKey::parse(key))
            .collect::<Result<Vec<_>>>()?;
        let set = config
            .set
            .iter()
            .map(|(key, value)| Ok((EnvironmentKey::parse(key)?, value.clone())))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let remove = config
            .remove
            .iter()
            .map(|key| EnvironmentKey::parse(key))
            .collect::<Result<Vec<_>>>()?;
        let inherited = inherit.iter().collect::<BTreeSet<_>>();
        if set.keys().any(|key| inherited.contains(key)) {
            bail!("gate environment key cannot be both inherited and set");
        }
        Ok(Self {
            inherit,
            set,
            remove,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceMutationPolicy {
    #[default]
    Forbid,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    Deny,
    Allow,
    #[default]
    UnenforcedDenyRequested,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateEvidenceGrade {
    #[default]
    IsolatedSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GateConfig {
    pub description: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: GateWorkingDirectoryConfig,
    pub environment: EnvironmentPolicyConfig,
    pub timeout_seconds: u64,
    pub output_limit_bytes: u64,
    pub success_codes: Vec<i32>,
    pub source_mutation: SourceMutationPolicy,
    pub network: NetworkPolicy,
    pub evidence_grade: GateEvidenceGrade,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            description: String::new(),
            program: String::new(),
            args: Vec::new(),
            cwd: GateWorkingDirectoryConfig::default(),
            environment: EnvironmentPolicyConfig::default(),
            timeout_seconds: DEFAULT_TIMEOUT_SECS,
            output_limit_bytes: DEFAULT_OUTPUT_LIMIT,
            success_codes: vec![0],
            source_mutation: SourceMutationPolicy::Forbid,
            network: NetworkPolicy::default(),
            evidence_grade: GateEvidenceGrade::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct GateGroupConfig {
    pub gates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct CatalogFile {
    gates: BTreeMap<String, GateConfig>,
    gate_groups: BTreeMap<String, GateGroupConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateSpec {
    pub id: GateId,
    pub description: String,
    pub program: Executable,
    pub args: Vec<Argument>,
    pub cwd: GateWorkingDirectory,
    pub environment: EnvironmentPolicy,
    pub timeout: Duration,
    pub output_limit: u64,
    pub success_codes: BTreeSet<i32>,
    pub source_mutation: SourceMutationPolicy,
    pub network: NetworkPolicy,
    pub evidence_grade: GateEvidenceGrade,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GateSpecDigest(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGate {
    spec: GateSpec,
    digest: GateSpecDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateSubject {
    pub repository: RepositoryRef,
    pub repository_point: RepositoryPointRef,
    pub tree_oid: TreeOid,
    pub consistency_token: ConsistencyToken,
}

#[derive(Debug)]
pub struct VerifiedGateSubject {
    subject: GateSubject,
    repo_root: PathBuf,
}

impl VerifiedGateSubject {
    pub fn subject(&self) -> &GateSubject {
        &self.subject
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }
}

pub fn verify_subject(repo_root: &Path) -> Result<VerifiedGateSubject> {
    let runner = crate::command::ProcessCommandRunner;
    let packet = crate::context::build_context_packet(&runner, repo_root, false)?;
    if packet.consistency.git_operation != crate::consistency::GitOperationState::None {
        bail!("cannot prepare gates during an in-progress Git operation");
    }
    if packet.consistency.conflict_state.has_conflicts {
        bail!("cannot prepare gates with unresolved conflicts");
    }
    let staged = packet
        .git
        .staged_files
        .iter()
        .map(|file| &file.path)
        .collect::<BTreeSet<_>>();
    if packet
        .git
        .unstaged_files
        .iter()
        .any(|file| staged.contains(&file.path))
    {
        bail!("cannot prepare gates when one path has both staged and unstaged changes");
    }
    let point = crate::semantic::repository_point(&runner, &packet)?;
    let (live, _) = crate::git::collect_git_state(&runner, repo_root)?;
    if !packet.consistency.verify(&live).is_consistent() {
        bail!("repository changed while the gate subject was being materialized");
    }
    Ok(VerifiedGateSubject {
        subject: GateSubject {
            repository: point.repository,
            repository_point: point.id,
            tree_oid: point.tree_oid,
            consistency_token: point.consistency_token,
        },
        repo_root: packet.git.repo_root,
    })
}

pub struct GateWorkspace {
    root: PathBuf,
    evidence_root: PathBuf,
    repo_root: PathBuf,
    lease_id: String,
    subject_commit: String,
}

impl GateWorkspace {
    pub fn materialize(subject: &VerifiedGateSubject) -> Result<Self> {
        let runner = crate::command::ProcessCommandRunner;
        let lease_id = unique_id("gate");
        crate::snapshot::acquire_lease(
            &runner,
            subject.repo_root(),
            &lease_id,
            &subject.subject.tree_oid.0,
            &subject.subject.consistency_token.0,
        )?;
        let root = match create_temp_dir(&lease_id) {
            Ok(root) => root,
            Err(error) => {
                let _ = crate::snapshot::release_lease(&runner, subject.repo_root(), &lease_id);
                return Err(error);
            }
        };
        let evidence_root = root.with_extension("evidence");
        if let Err(error) = fs::create_dir(&evidence_root) {
            let _ = fs::remove_dir_all(&root);
            let _ = crate::snapshot::release_lease(&runner, subject.repo_root(), &lease_id);
            return Err(error).with_context(|| {
                format!(
                    "failed to create gate evidence directory {}",
                    evidence_root.display()
                )
            });
        }
        let subject_commit = match materialize_repository(&runner, subject, &root, &lease_id)
            .and_then(|commit| {
                validate_workspace_symlinks(&root)?;
                Ok(commit)
            }) {
            Ok(commit) => commit,
            Err(error) => {
                let _ = fs::remove_dir_all(&root);
                let _ = fs::remove_dir_all(&evidence_root);
                let _ = crate::snapshot::release_lease(&runner, subject.repo_root(), &lease_id);
                return Err(error);
            }
        };
        Ok(Self {
            root,
            evidence_root,
            repo_root: subject.repo_root.clone(),
            lease_id,
            subject_commit,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn source_tree(&self) -> Result<String> {
        validate_standalone_repository(&self.root)?;
        let output = crate::command::ProcessCommandRunner.run(
            &self.root,
            "git",
            &["update-ref", "--no-deref", "HEAD", &self.subject_commit],
        )?;
        if output.status != 0 {
            bail!(
                "failed to restore gate snapshot HEAD: {}",
                output.stderr_lossy().trim()
            );
        }
        crate::snapshot::materialize(&crate::command::ProcessCommandRunner, &self.root)
    }

    fn output_path(&self, stream: &str) -> PathBuf {
        self.evidence_root.join(stream)
    }

    pub fn changed_paths(&self, before: &str, after: &str) -> Result<Vec<PathBuf>> {
        if before == after {
            return Ok(Vec::new());
        }
        let output = crate::command::ProcessCommandRunner
            .run(
                &self.root,
                "git",
                &["diff", "--name-only", "-z", before, after],
            )
            .context("failed to inspect gate workspace mutation")?;
        if output.status != 0 {
            bail!(
                "failed to inspect gate workspace mutation: {}",
                output.stderr_lossy().trim()
            );
        }
        Ok(output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(crate::path_codec::from_git_bytes)
            .collect())
    }
}

impl Drop for GateWorkspace {
    fn drop(&mut self) {
        let runner = crate::command::ProcessCommandRunner;
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(&self.evidence_root);
        let _ = crate::snapshot::release_lease(&runner, &self.repo_root, &self.lease_id);
    }
}

const OUTPUT_TAIL_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputEvidence {
    pub digest: String,
    pub bytes_observed: u64,
    pub truncated: bool,
    pub tail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentFingerprint {
    pub operating_system: String,
    pub architecture: String,
    pub executable: String,
    pub selected_variables_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateExecution {
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub stdout: OutputEvidence,
    pub stderr: OutputEvidence,
    pub environment: EnvironmentFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassedGate {
    gate: GateId,
    gate_spec_digest: GateSpecDigest,
    subject: GateSubject,
    execution: GateExecution,
    evidence_grade: GateEvidenceGrade,
}

impl PassedGate {
    pub fn gate(&self) -> &GateId {
        &self.gate
    }

    pub fn subject(&self) -> &GateSubject {
        &self.subject
    }

    pub fn execution(&self) -> &GateExecution {
        &self.execution
    }

    pub fn into_receipt(self) -> Result<GateReceipt> {
        let exit_code = self
            .execution
            .exit_code
            .context("a passing gate has no exit code")?;
        let content = ReceiptContent {
            gate: &self.gate,
            gate_spec_digest: &self.gate_spec_digest,
            subject: &self.subject,
            started_at: &self.execution.started_at,
            finished_at: &self.execution.finished_at,
            duration_ms: self.execution.duration_ms,
            exit_code,
            stdout: &self.execution.stdout,
            stderr: &self.execution.stderr,
            environment: &self.execution.environment,
            evidence_grade: self.evidence_grade,
        };
        let receipt_digest = digest_json(&content)?;
        let id = GateReceiptRef::parse(format!(
            "gatercpt_{}",
            receipt_digest
                .strip_prefix("sha256:")
                .unwrap_or(&receipt_digest)
        ))
        .map_err(anyhow::Error::from)?;
        Ok(GateReceipt {
            id,
            gate: self.gate,
            gate_spec_digest: self.gate_spec_digest,
            subject: self.subject,
            started_at: self.execution.started_at,
            finished_at: self.execution.finished_at,
            duration_ms: self.execution.duration_ms,
            exit_code,
            stdout: self.execution.stdout,
            stderr: self.execution.stderr,
            environment: self.execution.environment,
            evidence_grade: self.evidence_grade,
            receipt_digest,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateReceipt {
    id: GateReceiptRef,
    gate: GateId,
    gate_spec_digest: GateSpecDigest,
    subject: GateSubject,
    started_at: String,
    finished_at: String,
    duration_ms: u64,
    exit_code: i32,
    stdout: OutputEvidence,
    stderr: OutputEvidence,
    environment: EnvironmentFingerprint,
    evidence_grade: GateEvidenceGrade,
    receipt_digest: String,
}

impl GateReceipt {
    pub fn id(&self) -> &GateReceiptRef {
        &self.id
    }

    pub fn gate(&self) -> &GateId {
        &self.gate
    }

    pub fn gate_spec_digest(&self) -> &GateSpecDigest {
        &self.gate_spec_digest
    }

    pub fn subject(&self) -> &GateSubject {
        &self.subject
    }

    pub fn receipt_digest(&self) -> &str {
        &self.receipt_digest
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    pub fn stdout(&self) -> &OutputEvidence {
        &self.stdout
    }

    pub fn stderr(&self) -> &OutputEvidence {
        &self.stderr
    }

    pub fn validate(&self) -> bool {
        let content = ReceiptContent {
            gate: &self.gate,
            gate_spec_digest: &self.gate_spec_digest,
            subject: &self.subject,
            started_at: &self.started_at,
            finished_at: &self.finished_at,
            duration_ms: self.duration_ms,
            exit_code: self.exit_code,
            stdout: &self.stdout,
            stderr: &self.stderr,
            environment: &self.environment,
            evidence_grade: self.evidence_grade,
        };
        let Ok(expected) = digest_json(&content) else {
            return false;
        };
        let expected_id = format!(
            "gatercpt_{}",
            expected.strip_prefix("sha256:").unwrap_or(&expected)
        );
        expected == self.receipt_digest && self.id.as_str() == expected_id
    }
}

#[derive(Serialize)]
struct ReceiptContent<'a> {
    gate: &'a GateId,
    gate_spec_digest: &'a GateSpecDigest,
    subject: &'a GateSubject,
    started_at: &'a str,
    finished_at: &'a str,
    duration_ms: u64,
    exit_code: i32,
    stdout: &'a OutputEvidence,
    stderr: &'a OutputEvidence,
    environment: &'a EnvironmentFingerprint,
    evidence_grade: GateEvidenceGrade,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptApplicability {
    Current,
    StaleRepositoryPoint,
    StaleTree,
    GateDefinitionChanged,
    RepositoryMismatch,
    InvalidReceipt,
}

pub fn receipt_applicability(
    receipt: &GateReceipt,
    current_subject: &GateSubject,
    current_spec: &ResolvedGate,
) -> ReceiptApplicability {
    if !receipt.validate() {
        ReceiptApplicability::InvalidReceipt
    } else if receipt.subject.repository != current_subject.repository {
        ReceiptApplicability::RepositoryMismatch
    } else if receipt.gate != current_spec.spec.id
        || receipt.gate_spec_digest != current_spec.digest
    {
        ReceiptApplicability::GateDefinitionChanged
    } else if receipt.subject.tree_oid != current_subject.tree_oid {
        ReceiptApplicability::StaleTree
    } else if receipt.subject.repository_point != current_subject.repository_point
        || receipt.subject.consistency_token != current_subject.consistency_token
    {
        ReceiptApplicability::StaleRepositoryPoint
    } else {
        ReceiptApplicability::Current
    }
}

pub fn persist_receipt(repo_root: &Path, receipt: &GateReceipt) -> Result<PathBuf> {
    if !receipt.validate() {
        bail!("refusing to persist an invalid gate receipt");
    }
    let directory = repo_root.join(".toimija/runs/gates");
    fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    let path = directory.join(format!("{}.json", receipt.id));
    let mut bytes = serde_json::to_vec_pretty(receipt)?;
    bytes.push(b'\n');
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read(&path)?;
            if existing != bytes {
                bail!("gate receipt path {} is not immutable", path.display());
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(path)
}

pub fn read_receipts(repo_root: &Path) -> Result<Vec<GateReceipt>> {
    const MAX_RECEIPT_BYTES: u64 = 1024 * 1024;
    let directory = repo_root.join(".toimija/runs/gates");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut receipts = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(OsStr::to_str) != Some("json") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("gate receipt must be a regular file: {}", path.display());
        }
        if metadata.len() > MAX_RECEIPT_BYTES {
            bail!("gate receipt exceeds the {MAX_RECEIPT_BYTES}-byte limit");
        }
        let bytes = fs::read(&path)?;
        let receipt: GateReceipt = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid gate receipt {}", path.display()))?;
        if !receipt.validate() {
            bail!("gate receipt digest mismatch: {}", path.display());
        }
        receipts.push(receipt);
    }
    receipts.sort_by(|left, right| left.started_at.cmp(&right.started_at));
    Ok(receipts)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedGate {
    pub gate: GateId,
    pub subject: GateSubject,
    pub execution: GateExecution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimedOutGate {
    pub gate: GateId,
    pub subject: GateSubject,
    pub execution: GateExecution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnFailure {
    pub gate: GateId,
    pub subject: GateSubject,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationEvidence {
    pub gate: GateId,
    pub subject: GateSubject,
    pub observed_tree: String,
    pub changed_paths: Vec<PathBuf>,
    pub execution: GateExecution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceFailure {
    pub gate: GateId,
    pub subject: GateSubject,
    pub reason: String,
    pub execution: GateExecution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cancellation {
    pub gate: GateId,
    pub subject: GateSubject,
    pub execution: GateExecution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum GateOutcome {
    Passed(PassedGate),
    Failed(FailedGate),
    TimedOut(TimedOutGate),
    SpawnFailed(SpawnFailure),
    SubjectMutated(MutationEvidence),
    EvidenceIncomplete(EvidenceFailure),
    Cancelled(Cancellation),
}

pub struct PreparedGate {
    gate: ResolvedGate,
    subject: GateSubject,
    workspace: GateWorkspace,
    cwd: PathBuf,
    executable: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    fingerprint: EnvironmentFingerprint,
}

impl PreparedGate {
    pub fn prepare(gate: ResolvedGate, subject: &VerifiedGateSubject) -> Result<Self> {
        if gate.spec.network == NetworkPolicy::Deny {
            bail!(
                "gate `{}` requires enforced network denial, which is unavailable",
                gate.spec.id
            );
        }
        let workspace = GateWorkspace::materialize(subject)?;
        let baseline = workspace.source_tree()?;
        if baseline != subject.subject.tree_oid.0 {
            bail!(
                "materialized gate workspace tree {baseline} does not match subject {}",
                subject.subject.tree_oid.0
            );
        }
        let cwd = match &gate.spec.cwd {
            GateWorkingDirectory::RepositoryRoot => workspace.root().to_path_buf(),
            GateWorkingDirectory::Relative(path) => workspace.root().join(path),
        };
        let root = workspace.root().canonicalize()?;
        let cwd = cwd
            .canonicalize()
            .with_context(|| format!("gate cwd {} does not exist", cwd.display()))?;
        if !cwd.starts_with(&root) || !cwd.is_dir() {
            bail!(
                "gate cwd {} escapes the materialized snapshot",
                cwd.display()
            );
        }
        let environment = selected_environment(&gate.spec.environment);
        let executable = resolve_executable(
            gate.spec.program.as_os_str(),
            environment.get(OsStr::new("PATH")),
            &cwd,
        )?;
        let fingerprint = EnvironmentFingerprint {
            operating_system: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            executable: executable.display().to_string(),
            selected_variables_digest: environment_digest(&environment),
        };
        Ok(Self {
            gate,
            subject: subject.subject.clone(),
            workspace,
            cwd,
            executable,
            environment,
            fingerprint,
        })
    }

    pub fn execute(self) -> GateOutcome {
        let cancelled = Arc::new(AtomicBool::new(false));
        match SignalRegistration::new(Arc::clone(&cancelled)) {
            Ok(_signals) => self.execute_with_cancellation(cancelled),
            Err(error) => GateOutcome::SpawnFailed(SpawnFailure {
                gate: self.gate.spec.id.clone(),
                subject: self.subject.clone(),
                message: format!("failed to install gate signal handlers: {error}"),
            }),
        }
    }

    fn execute_with_cancellation(self, cancelled: Arc<AtomicBool>) -> GateOutcome {
        let started_at = timestamp();
        let started = std::time::Instant::now();
        let mut command = Command::new(&self.executable);
        command
            .args(self.gate.spec.args.iter().map(Argument::as_os_str))
            .current_dir(&self.cwd)
            .env_clear()
            .envs(&self.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = match command.spawn() {
            Ok(child) => ChildGroup::new(child),
            Err(error) => {
                return GateOutcome::SpawnFailed(SpawnFailure {
                    gate: self.gate.spec.id.clone(),
                    subject: self.subject.clone(),
                    message: error.to_string(),
                });
            }
        };
        let stdout = child
            .child
            .stdout
            .take()
            .expect("gate stdout was configured as piped");
        let stderr = child
            .child
            .stderr
            .take()
            .expect("gate stderr was configured as piped");
        let limit = self.gate.spec.output_limit;
        let stdout_path = self.workspace.output_path("stdout");
        let stderr_path = self.workspace.output_path("stderr");
        let stdout_reader = std::thread::spawn(move || capture_stream(stdout, &stdout_path, limit));
        let stderr_reader = std::thread::spawn(move || capture_stream(stderr, &stderr_path, limit));

        let deadline = started + self.gate.spec.timeout;
        let (status, timed_out, was_cancelled) = loop {
            match child.child.try_wait() {
                Ok(Some(status)) => break (Ok(status), false, false),
                Ok(None) if cancelled.load(Ordering::Relaxed) => {
                    child.terminate();
                    break (child.child.wait(), false, true);
                }
                Ok(None) if std::time::Instant::now() >= deadline => {
                    child.terminate();
                    break (child.child.wait(), true, false);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => break (Err(error), false, false),
            }
        };
        child.reaped = status.is_ok();
        if !timed_out && !was_cancelled {
            child.terminate();
        }
        let stdout = join_capture(stdout_reader, "stdout");
        let stderr = join_capture(stderr_reader, "stderr");
        let exit_code = status
            .as_ref()
            .ok()
            .and_then(std::process::ExitStatus::code);
        let execution = GateExecution {
            started_at,
            finished_at: timestamp(),
            duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            exit_code,
            stdout: stdout
                .as_ref()
                .cloned()
                .unwrap_or_else(|error| failed_output(error)),
            stderr: stderr
                .as_ref()
                .cloned()
                .unwrap_or_else(|error| failed_output(error)),
            environment: self.fingerprint.clone(),
        };
        let observed_tree = self.workspace.source_tree();
        let observed_tree = match observed_tree {
            Ok(tree) => tree,
            Err(error) => {
                return GateOutcome::EvidenceIncomplete(EvidenceFailure {
                    gate: self.gate.spec.id.clone(),
                    subject: self.subject.clone(),
                    reason: format!("post-run subject verification failed: {error:#}"),
                    execution,
                });
            }
        };
        if observed_tree != self.subject.tree_oid.0 {
            let changed_paths = self
                .workspace
                .changed_paths(&self.subject.tree_oid.0, &observed_tree)
                .unwrap_or_default();
            return GateOutcome::SubjectMutated(MutationEvidence {
                gate: self.gate.spec.id.clone(),
                subject: self.subject.clone(),
                observed_tree,
                changed_paths,
                execution,
            });
        }
        if was_cancelled {
            return GateOutcome::Cancelled(Cancellation {
                gate: self.gate.spec.id.clone(),
                subject: self.subject.clone(),
                execution,
            });
        }
        if timed_out {
            return GateOutcome::TimedOut(TimedOutGate {
                gate: self.gate.spec.id.clone(),
                subject: self.subject.clone(),
                execution,
            });
        }
        if let Err(error) = status {
            return GateOutcome::EvidenceIncomplete(EvidenceFailure {
                gate: self.gate.spec.id.clone(),
                subject: self.subject.clone(),
                reason: format!("failed while waiting for gate: {error}"),
                execution,
            });
        }
        if let Err(error) = stdout.or(stderr) {
            return GateOutcome::EvidenceIncomplete(EvidenceFailure {
                gate: self.gate.spec.id.clone(),
                subject: self.subject.clone(),
                reason: error,
                execution,
            });
        }
        if execution.stdout.truncated || execution.stderr.truncated {
            return GateOutcome::EvidenceIncomplete(EvidenceFailure {
                gate: self.gate.spec.id.clone(),
                subject: self.subject.clone(),
                reason: "gate output exceeded the configured evidence limit".into(),
                execution,
            });
        }
        if execution
            .exit_code
            .is_some_and(|code| self.gate.spec.success_codes.contains(&code))
        {
            GateOutcome::Passed(PassedGate {
                gate: self.gate.spec.id,
                gate_spec_digest: self.gate.digest,
                subject: self.subject,
                execution,
                evidence_grade: self.gate.spec.evidence_grade,
            })
        } else {
            GateOutcome::Failed(FailedGate {
                gate: self.gate.spec.id,
                subject: self.subject,
                execution,
            })
        }
    }
}

#[derive(Debug, Clone)]
struct CapturedStream {
    evidence: OutputEvidence,
}

fn capture_stream(
    mut input: impl Read,
    path: &Path,
    limit: u64,
) -> std::result::Result<CapturedStream, String> {
    let mut file = fs::File::create(path).map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    let mut observed = 0_u64;
    let mut written = 0_u64;
    let mut tail = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = input.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        let bytes = &buffer[..read];
        digest.update(bytes);
        observed = observed.saturating_add(read as u64);
        tail.extend_from_slice(bytes);
        if tail.len() > OUTPUT_TAIL_BYTES {
            tail.drain(..tail.len() - OUTPUT_TAIL_BYTES);
        }
        let remaining = limit.saturating_sub(written).min(read as u64) as usize;
        if remaining > 0 {
            file.write_all(&bytes[..remaining])
                .map_err(|error| error.to_string())?;
            written += remaining as u64;
        }
    }
    file.flush().map_err(|error| error.to_string())?;
    Ok(CapturedStream {
        evidence: OutputEvidence {
            digest: format!("sha256:{:x}", digest.finalize()),
            bytes_observed: observed,
            truncated: observed > limit,
            tail: String::from_utf8_lossy(&tail).into_owned(),
            artifact: None,
        },
    })
}

fn join_capture(
    handle: std::thread::JoinHandle<std::result::Result<CapturedStream, String>>,
    stream: &str,
) -> std::result::Result<OutputEvidence, String> {
    handle
        .join()
        .map_err(|_| format!("{stream} capture thread panicked"))?
        .map(|capture| capture.evidence)
}

fn failed_output(reason: &str) -> OutputEvidence {
    OutputEvidence {
        digest: String::new(),
        bytes_observed: 0,
        truncated: true,
        tail: reason.to_string(),
        artifact: None,
    }
}

struct ChildGroup {
    child: Child,
    reaped: bool,
}

impl ChildGroup {
    fn new(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }

    fn terminate(&mut self) {
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(self.child.id()) {
            // SAFETY: process_group(0) made the child PID its process-group ID.
            unsafe {
                libc::kill(-pid, libc::SIGTERM);
            }
            let deadline = std::time::Instant::now() + Duration::from_millis(250);
            while std::time::Instant::now() < deadline {
                if self.child.try_wait().ok().flatten().is_some() {
                    self.reaped = true;
                }
                // SAFETY: signal 0 probes the dedicated process group.
                if unsafe { libc::kill(-pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            // SAFETY: same dedicated process group as above.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        if !self.reaped {
            let _ = self.child.kill();
        }
    }
}

impl Drop for ChildGroup {
    fn drop(&mut self) {
        if !self.reaped {
            self.terminate();
            let _ = self.child.wait();
        }
    }
}

struct SignalRegistration {
    #[cfg(unix)]
    ids: Vec<signal_hook::SigId>,
}

impl SignalRegistration {
    fn new(cancelled: Arc<AtomicBool>) -> Result<Self> {
        #[cfg(unix)]
        {
            let ids = [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM]
                .into_iter()
                .map(|signal| signal_hook::flag::register(signal, Arc::clone(&cancelled)))
                .collect::<std::io::Result<Vec<_>>>()?;
            Ok(Self { ids })
        }
        #[cfg(not(unix))]
        {
            let _ = cancelled;
            Ok(Self {})
        }
    }
}

impl Drop for SignalRegistration {
    fn drop(&mut self) {
        #[cfg(unix)]
        for id in self.ids.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

fn selected_environment(policy: &EnvironmentPolicy) -> BTreeMap<OsString, OsString> {
    let mut environment = BTreeMap::new();
    for key in &policy.inherit {
        if let Some(value) = std::env::var_os(key.as_str()) {
            environment.insert(key.as_str().into(), value);
        }
    }
    for (key, value) in &policy.set {
        environment.insert(key.as_str().into(), value.into());
    }
    for key in &policy.remove {
        environment.remove(OsStr::new(key.as_str()));
    }
    environment
}

fn resolve_executable(program: &OsStr, path: Option<&OsString>, cwd: &Path) -> Result<PathBuf> {
    which::which_in(program, path, cwd).with_context(|| {
        format!(
            "gate executable `{}` was not found",
            program.to_string_lossy()
        )
    })
}

fn environment_digest(environment: &BTreeMap<OsString, OsString>) -> String {
    let mut digest = Sha256::new();
    for (key, value) in environment {
        digest.update(key.as_encoded_bytes());
        digest.update([0]);
        digest.update(value.as_encoded_bytes());
        digest.update([0xff]);
    }
    format!("sha256:{:x}", digest.finalize())
}

fn digest_json(value: &impl Serialize) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(
            false_agent_protocol::digest::canonical_json(value).map_err(anyhow::Error::from)?
        )
    ))
}

fn timestamp() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

impl ResolvedGate {
    pub fn spec(&self) -> &GateSpec {
        &self.spec
    }

    pub fn digest(&self) -> &GateSpecDigest {
        &self.digest
    }
}

#[derive(Debug, Clone)]
pub struct GateCatalog {
    gates: BTreeMap<GateId, ResolvedGate>,
    groups: BTreeMap<GateId, Vec<GateId>>,
}

impl GateCatalog {
    pub fn load(repo_root: &Path, config: &crate::config::RepoConfig) -> Result<Self> {
        let mut gates = config.gates.clone();
        let mut groups = config.gate_groups.clone();
        if let Some(path) = user_catalog_path().filter(|path| path.is_file()) {
            let source = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let user: CatalogFile = toml::from_str(&source)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            gates.extend(user.gates);
            groups.extend(user.gate_groups);
        }
        Self::from_configs(repo_root, gates, groups)
    }

    fn from_configs(
        _repo_root: &Path,
        configs: BTreeMap<String, GateConfig>,
        group_configs: BTreeMap<String, GateGroupConfig>,
    ) -> Result<Self> {
        let mut gates = BTreeMap::new();
        for (id, config) in configs {
            let id = GateId::parse(id)?;
            let spec = GateSpec::from_config(id.clone(), config)?;
            let digest = spec.digest()?;
            gates.insert(id, ResolvedGate { spec, digest });
        }
        let mut raw_groups = BTreeMap::new();
        for (id, group) in group_configs {
            let id = GateId::parse(id)?;
            if gates.contains_key(&id) {
                bail!("gate group `{id}` conflicts with a gate of the same name");
            }
            if group.gates.is_empty() {
                bail!("gate group `{id}` must contain at least one gate");
            }
            raw_groups.insert(
                id,
                group
                    .gates
                    .into_iter()
                    .map(GateId::parse)
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        let mut groups = BTreeMap::new();
        for id in raw_groups.keys() {
            let mut expanded = Vec::new();
            expand_group(id, &gates, &raw_groups, &mut BTreeSet::new(), &mut expanded)?;
            let mut seen = BTreeSet::new();
            expanded.retain(|gate| seen.insert(gate.clone()));
            groups.insert(id.clone(), expanded);
        }
        Ok(Self { gates, groups })
    }

    pub fn gates(&self) -> impl Iterator<Item = &ResolvedGate> {
        self.gates.values()
    }

    pub fn groups(&self) -> impl Iterator<Item = (&GateId, &[GateId])> {
        self.groups.iter().map(|(id, gates)| (id, gates.as_slice()))
    }

    pub fn inspect(&self, id: &GateId) -> Option<CatalogEntry<'_>> {
        self.gates
            .get(id)
            .map(CatalogEntry::Gate)
            .or_else(|| self.groups.get(id).map(|gates| CatalogEntry::Group(gates)))
    }

    pub fn resolve(&self, id: &GateId) -> Result<Vec<ResolvedGate>> {
        if let Some(gate) = self.gates.get(id) {
            return Ok(vec![gate.clone()]);
        }
        if let Some(group) = self.groups.get(id) {
            return group
                .iter()
                .map(|gate| {
                    self.gates.get(gate).cloned().with_context(|| {
                        format!("gate group `{id}` references missing gate `{gate}`")
                    })
                })
                .collect();
        }
        bail!("unknown gate `{id}`")
    }
}

pub enum CatalogEntry<'a> {
    Gate(&'a ResolvedGate),
    Group(&'a [GateId]),
}

impl GateSpec {
    fn from_config(id: GateId, config: GateConfig) -> Result<Self> {
        if config.timeout_seconds == 0 {
            bail!("gate `{id}` timeout must be greater than zero");
        }
        if config.output_limit_bytes == 0 {
            bail!("gate `{id}` output limit must be greater than zero");
        }
        if config.success_codes.is_empty() {
            bail!("gate `{id}` success code set must not be empty");
        }
        Ok(Self {
            id,
            description: config.description,
            program: Executable::new(&config.program)?,
            args: config
                .args
                .iter()
                .map(|value| Argument::new(value))
                .collect(),
            cwd: GateWorkingDirectory::from_config(&config.cwd)?,
            environment: EnvironmentPolicy::from_config(&config.environment)?,
            timeout: Duration::from_secs(config.timeout_seconds),
            output_limit: config.output_limit_bytes,
            success_codes: config.success_codes.into_iter().collect(),
            source_mutation: config.source_mutation,
            network: config.network,
            evidence_grade: config.evidence_grade,
        })
    }

    fn digest(&self) -> Result<GateSpecDigest> {
        #[derive(Serialize)]
        struct Canonical<'a> {
            program: String,
            args: Vec<String>,
            cwd: String,
            inherit: Vec<&'a str>,
            set: BTreeMap<&'a str, &'a str>,
            remove: Vec<&'a str>,
            timeout_millis: u128,
            output_limit: u64,
            success_codes: Vec<i32>,
            source_mutation: SourceMutationPolicy,
            network: NetworkPolicy,
            evidence_grade: GateEvidenceGrade,
        }
        let canonical = Canonical {
            program: self.program.0.to_string_lossy().into_owned(),
            args: self
                .args
                .iter()
                .map(|arg| arg.0.to_string_lossy().into_owned())
                .collect(),
            cwd: match &self.cwd {
                GateWorkingDirectory::RepositoryRoot => ".".into(),
                GateWorkingDirectory::Relative(path) => path.to_string_lossy().into_owned(),
            },
            inherit: self
                .environment
                .inherit
                .iter()
                .map(EnvironmentKey::as_str)
                .collect(),
            set: self
                .environment
                .set
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect(),
            remove: self
                .environment
                .remove
                .iter()
                .map(EnvironmentKey::as_str)
                .collect(),
            timeout_millis: self.timeout.as_millis(),
            output_limit: self.output_limit,
            success_codes: self.success_codes.iter().copied().collect(),
            source_mutation: self.source_mutation,
            network: self.network,
            evidence_grade: self.evidence_grade,
        };
        let bytes = serde_json::to_vec(&canonical)?;
        Ok(GateSpecDigest(format!(
            "sha256:{:x}",
            Sha256::digest(bytes)
        )))
    }
}

fn expand_group(
    id: &GateId,
    gates: &BTreeMap<GateId, ResolvedGate>,
    groups: &BTreeMap<GateId, Vec<GateId>>,
    visiting: &mut BTreeSet<GateId>,
    expanded: &mut Vec<GateId>,
) -> Result<()> {
    if !visiting.insert(id.clone()) {
        bail!("gate group cycle includes `{id}`");
    }
    for member in groups
        .get(id)
        .with_context(|| format!("unknown gate group `{id}`"))?
    {
        if gates.contains_key(member) {
            expanded.push(member.clone());
        } else if groups.contains_key(member) {
            expand_group(member, gates, groups, visiting, expanded)?;
        } else {
            bail!("gate group `{id}` references unknown gate or group `{member}`");
        }
    }
    visiting.remove(id);
    Ok(())
}

fn materialize_repository(
    runner: &impl CommandRunner,
    subject: &VerifiedGateSubject,
    root: &Path,
    id: &str,
) -> Result<String> {
    let identity = [
        ("GIT_AUTHOR_NAME", OsStr::new("Toimija Gate Authority")),
        ("GIT_AUTHOR_EMAIL", OsStr::new("gate@localhost")),
        ("GIT_AUTHOR_DATE", OsStr::new("1970-01-01T00:00:00Z")),
        ("GIT_COMMITTER_NAME", OsStr::new("Toimija Gate Authority")),
        ("GIT_COMMITTER_EMAIL", OsStr::new("gate@localhost")),
        ("GIT_COMMITTER_DATE", OsStr::new("1970-01-01T00:00:00Z")),
    ];
    let commit = runner
        .run_with_env(
            subject.repo_root(),
            "git",
            &["commit-tree", &subject.subject.tree_oid.0],
            &identity,
        )
        .context("failed to create the temporary snapshot commit")?;
    if commit.status != 0 {
        bail!(
            "failed to create the temporary snapshot commit: {}",
            commit.stderr_lossy().trim()
        );
    }
    let commit = commit.stdout_lossy();
    let commit = commit.trim();
    let root_text = root
        .to_str()
        .context("temporary gate workspace path is not UTF-8")?;
    let repo_text = subject
        .repo_root()
        .to_str()
        .context("repository path is not UTF-8")?;
    let temporary_ref = format!("refs/toimija/gates/{id}");
    let pinned = runner.run(
        subject.repo_root(),
        "git",
        &["update-ref", &temporary_ref, commit],
    )?;
    if pinned.status != 0 {
        bail!(
            "failed to pin the temporary snapshot commit: {}",
            pinned.stderr_lossy().trim()
        );
    }
    let result = (|| {
        let initialized =
            runner.run(subject.repo_root(), "git", &["init", "--quiet", root_text])?;
        if initialized.status != 0 {
            bail!(
                "failed to initialize the standalone gate repository: {}",
                initialized.stderr_lossy().trim()
            );
        }
        let refspec = format!("{temporary_ref}:refs/toimija/subject");
        let fetched = runner.run(
            root,
            "git",
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-write-fetch-head",
                repo_text,
                &refspec,
            ],
        )?;
        if fetched.status != 0 {
            bail!(
                "failed to fetch the gate snapshot: {}",
                fetched.stderr_lossy().trim()
            );
        }
        let checked_out = runner.run(
            root,
            "git",
            &["checkout", "--quiet", "--detach", "refs/toimija/subject"],
        )?;
        if checked_out.status != 0 {
            bail!(
                "failed to check out the gate snapshot: {}",
                checked_out.stderr_lossy().trim()
            );
        }
        Ok(commit.to_string())
    })();
    let _ = runner.run(
        subject.repo_root(),
        "git",
        &["update-ref", "-d", &temporary_ref],
    );
    result
}

fn validate_standalone_repository(root: &Path) -> Result<()> {
    let canonical_root = root.canonicalize()?;
    let git_dir = root.join(".git");
    let metadata = fs::symlink_metadata(&git_dir)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("gate snapshot Git metadata is not a standalone directory");
    }
    let canonical_git_dir = git_dir.canonicalize()?;
    if !canonical_git_dir.starts_with(&canonical_root) {
        bail!("gate snapshot Git metadata escapes the workspace");
    }
    let output = crate::command::ProcessCommandRunner.run(
        root,
        "git",
        &["rev-parse", "--show-toplevel", "--absolute-git-dir"],
    )?;
    if output.status != 0 {
        bail!(
            "failed to verify the standalone gate repository: {}",
            output.stderr_lossy().trim()
        );
    }
    let stdout = output.stdout_lossy();
    let mut lines = stdout.lines().map(PathBuf::from);
    let top = lines
        .next()
        .context("gate repository omitted its worktree root")?
        .canonicalize()?;
    let reported_git_dir = lines
        .next()
        .context("gate repository omitted its Git directory")?
        .canonicalize()?;
    if top != canonical_root || reported_git_dir != canonical_git_dir {
        bail!("gate snapshot repository redirects outside the workspace");
    }
    Ok(())
}

fn create_temp_dir(id: &str) -> Result<PathBuf> {
    let root = std::env::temp_dir().join(format!("toimija-{id}"));
    fs::create_dir(&root)
        .with_context(|| format!("failed to create gate workspace {}", root.display()))?;
    Ok(root)
}

fn unique_id(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{}-{nanos}", std::process::id())
}

fn validate_workspace_symlinks(root: &Path) -> Result<()> {
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", root.display()))?;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("failed to read {}", directory.display()))?
        {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                let target = path.canonicalize().with_context(|| {
                    format!(
                        "gate workspace contains dangling symlink {}",
                        path.display()
                    )
                })?;
                if !target.starts_with(&canonical_root) {
                    bail!(
                        "gate workspace symlink {} escapes the snapshot",
                        path.display()
                    );
                }
            } else if metadata.is_dir() {
                pending.push(path);
            }
        }
    }
    Ok(())
}

fn user_catalog_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|root| root.join("toimija/gates.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandRunner, ProcessCommandRunner};

    fn config(program: &str, args: &[&str]) -> GateConfig {
        GateConfig {
            description: "cosmetic".into(),
            program: program.into(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            ..GateConfig::default()
        }
    }

    fn catalog(gates: &[(&str, GateConfig)], groups: &[(&str, &[&str])]) -> Result<GateCatalog> {
        GateCatalog::from_configs(
            Path::new("."),
            gates
                .iter()
                .map(|(id, config)| ((*id).to_string(), config.clone()))
                .collect(),
            groups
                .iter()
                .map(|(id, members)| {
                    (
                        (*id).to_string(),
                        GateGroupConfig {
                            gates: members.iter().map(|member| (*member).to_string()).collect(),
                        },
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn validates_ids_direct_commands_and_specs() {
        let loaded = catalog(&[("rust-test", config("cargo", &["test"]))], &[]).unwrap();
        let gate = loaded
            .resolve(&GateId::parse("rust-test").unwrap())
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(gate.spec().program.to_string(), "cargo");
        assert_eq!(gate.spec().args[0].as_os_str(), "test");
        assert!(GateId::parse("../test").is_err());
        assert!(catalog(&[("shell", config("sh", &["-c", "echo no"]))], &[]).is_err());
    }

    #[test]
    fn rejects_unsafe_cwd_and_empty_success_codes() {
        let mut unsafe_cwd = config("cargo", &["test"]);
        unsafe_cwd.cwd = GateWorkingDirectoryConfig::Relative(PathBuf::from("../escape"));
        assert!(catalog(&[("test", unsafe_cwd)], &[]).is_err());
        let mut no_codes = config("cargo", &["test"]);
        no_codes.success_codes.clear();
        assert!(catalog(&[("test", no_codes)], &[]).is_err());
    }

    #[test]
    fn groups_expand_once_and_cycles_fail() {
        let loaded = catalog(
            &[
                ("format", config("cargo", &["fmt", "--check"])),
                ("test", config("cargo", &["test"])),
            ],
            &[
                ("quick", &["format"]),
                ("full", &["quick", "test", "format"]),
            ],
        )
        .unwrap();
        let resolved = loaded.resolve(&GateId::parse("full").unwrap()).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].spec().id.as_str(), "format");
        assert!(catalog(&[], &[("a", &["b"]), ("b", &["a"])]).is_err());
    }

    #[test]
    fn spec_digest_is_deterministic_and_execution_sensitive_only() {
        let first = catalog(&[("test", config("cargo", &["test"]))], &[])
            .unwrap()
            .resolve(&GateId::parse("test").unwrap())
            .unwrap()
            .pop()
            .unwrap();
        let mut cosmetic = config("cargo", &["test"]);
        cosmetic.description = "different words".into();
        let second = catalog(&[("test", cosmetic)], &[])
            .unwrap()
            .resolve(&GateId::parse("test").unwrap())
            .unwrap()
            .pop()
            .unwrap();
        let changed = catalog(&[("test", config("cargo", &["test", "--all"]))], &[])
            .unwrap()
            .resolve(&GateId::parse("test").unwrap())
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(first.digest(), second.digest());
        assert_ne!(first.digest(), changed.digest());
    }

    fn git(repo: &Path, args: &[&str]) {
        let output = ProcessCommandRunner.run(repo, "git", args).unwrap();
        assert_eq!(output.status, 0, "git {args:?}: {}", output.stderr_lossy());
    }

    fn repository() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        git(temp.path(), &["init", "-q"]);
        git(temp.path(), &["config", "user.name", "Gate Test"]);
        git(
            temp.path(),
            &["config", "user.email", "gate-test@localhost"],
        );
        std::fs::write(temp.path().join("source.txt"), "one\n").unwrap();
        git(temp.path(), &["add", "source.txt"]);
        git(temp.path(), &["commit", "-q", "-m", "initial"]);
        temp
    }

    #[cfg(unix)]
    fn write_gate(repo: &Path, source: &str) {
        use std::os::unix::fs::PermissionsExt;

        let path = repo.join("gate.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{source}\n")).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    fn prepared_gate(
        repo: &Path,
        source: &str,
        args: &[String],
        output_limit: u64,
    ) -> PreparedGate {
        write_gate(repo, source);
        let mut gate = config("./gate.sh", &[]);
        gate.args = args.to_vec();
        gate.output_limit_bytes = output_limit;
        let resolved = catalog(&[("check", gate)], &[])
            .unwrap()
            .resolve(&GateId::parse("check").unwrap())
            .unwrap()
            .pop()
            .unwrap();
        let subject = verify_subject(repo).unwrap();
        PreparedGate::prepare(resolved, &subject).unwrap()
    }

    #[cfg(unix)]
    fn receipt(repo: &Path) -> (GateReceipt, ResolvedGate, VerifiedGateSubject) {
        write_gate(repo, "printf ok");
        let resolved = catalog(&[("check", config("./gate.sh", &[]))], &[])
            .unwrap()
            .resolve(&GateId::parse("check").unwrap())
            .unwrap()
            .pop()
            .unwrap();
        let subject = verify_subject(repo).unwrap();
        let outcome = PreparedGate::prepare(resolved.clone(), &subject)
            .unwrap()
            .execute();
        let GateOutcome::Passed(passed) = outcome else {
            panic!("expected pass");
        };
        (passed.into_receipt().unwrap(), resolved, subject)
    }

    #[test]
    fn subject_distinguishes_dirty_states_at_the_same_head() {
        let repo = repository();
        let first = verify_subject(repo.path()).unwrap();
        std::fs::write(repo.path().join("source.txt"), "two\n").unwrap();
        let second = verify_subject(repo.path()).unwrap();

        assert_ne!(first.subject.tree_oid, second.subject.tree_oid);
        assert_ne!(
            first.subject.consistency_token,
            second.subject.consistency_token
        );
        assert_ne!(
            first.subject.repository_point,
            second.subject.repository_point
        );
    }

    #[test]
    fn workspace_is_outside_checkout_and_mutation_is_isolated() {
        let repo = repository();
        let subject = verify_subject(repo.path()).unwrap();
        let workspace = GateWorkspace::materialize(&subject).unwrap();
        let root = workspace.root().to_path_buf();

        assert!(!root.starts_with(repo.path()));
        assert_eq!(
            std::fs::read_to_string(root.join("source.txt")).unwrap(),
            "one\n"
        );
        std::fs::write(root.join("source.txt"), "changed\n").unwrap();
        let after = workspace.source_tree().unwrap();
        assert_ne!(after, subject.subject.tree_oid.0);
        assert_eq!(
            workspace
                .changed_paths(&subject.subject.tree_oid.0, &after)
                .unwrap(),
            vec![PathBuf::from("source.txt")]
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("source.txt")).unwrap(),
            "one\n"
        );
        drop(workspace);
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn workspace_does_not_share_repository_metadata() {
        let repo = repository();
        let outcome = prepared_gate(
            repo.path(),
            "git update-ref refs/heads/gate-mutated HEAD",
            &[],
            1024,
        )
        .execute();
        assert!(matches!(outcome, GateOutcome::Passed(_)));

        let original = ProcessCommandRunner
            .run(
                repo.path(),
                "git",
                &["show-ref", "--verify", "--quiet", "refs/heads/gate-mutated"],
            )
            .unwrap();
        assert_ne!(original.status, 0);
    }

    #[cfg(unix)]
    #[test]
    fn workspace_rejects_symlinks_that_escape_the_snapshot() {
        use std::os::unix::fs::symlink;

        let repo = repository();
        symlink("/tmp", repo.path().join("escape")).unwrap();
        git(repo.path(), &["add", "escape"]);
        let subject = verify_subject(repo.path()).unwrap();
        let error = GateWorkspace::materialize(&subject).err().unwrap();
        assert!(error.to_string().contains("escapes the snapshot"));
    }

    #[cfg(unix)]
    #[test]
    fn execution_captures_separate_complete_output() {
        let repo = repository();
        let outcome = prepared_gate(repo.path(), "printf out; printf err >&2", &[], 1024).execute();
        let GateOutcome::Passed(passed) = outcome else {
            panic!("expected pass");
        };
        assert_eq!(passed.execution.stdout.bytes_observed, 3);
        assert_eq!(passed.execution.stdout.tail, "out");
        assert_eq!(passed.execution.stderr.tail, "err");
        assert_eq!(
            passed.execution.stdout.digest,
            format!("sha256:{:x}", Sha256::digest(b"out"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn execution_uses_the_resolved_executable() {
        let repo = repository();
        let mut prepared = prepared_gate(repo.path(), "exit 0", &[], 1024);
        prepared.gate.spec.program =
            Executable::new("toimija-gate-program-does-not-exist").unwrap();
        assert!(matches!(prepared.execute(), GateOutcome::Passed(_)));
    }

    #[cfg(unix)]
    #[test]
    fn successful_gate_reaps_descendants_before_capturing_output() {
        let repo = repository();
        let prepared = prepared_gate(repo.path(), "sh -c 'trap \"\" TERM; sleep 4' &", &[], 1024);
        let started = std::time::Instant::now();
        let outcome = prepared.execute();

        assert!(matches!(outcome, GateOutcome::Passed(_)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn nonzero_spawn_failure_and_cancellation_are_distinct() {
        let repo = repository();
        let failed = prepared_gate(repo.path(), "printf bad >&2; exit 7", &[], 1024).execute();
        assert!(matches!(failed, GateOutcome::Failed(_)));

        let prepared = prepared_gate(repo.path(), "sleep 30", &[], 1024);
        std::fs::remove_file(prepared.workspace.root().join("gate.sh")).unwrap();
        assert!(matches!(prepared.execute(), GateOutcome::SpawnFailed(_)));

        let prepared = prepared_gate(repo.path(), "sleep 30", &[], 1024);
        let cancelled = Arc::new(AtomicBool::new(true));
        assert!(matches!(
            prepared.execute_with_cancellation(cancelled),
            GateOutcome::Cancelled(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn oversized_output_is_incomplete_and_cannot_pass() {
        let repo = repository();
        let outcome = prepared_gate(
            repo.path(),
            "i=0; while [ \"$i\" -lt 100 ]; do printf 0123456789; i=$((i + 1)); done",
            &[],
            32,
        )
        .execute();
        let GateOutcome::EvidenceIncomplete(failure) = outcome else {
            panic!("expected incomplete evidence");
        };
        assert_eq!(failure.execution.stdout.bytes_observed, 1000);
        assert!(failure.execution.stdout.truncated);
    }

    #[cfg(unix)]
    #[test]
    fn source_mutation_fails_but_ignored_build_output_does_not() {
        let repo = repository();
        let outcome =
            prepared_gate(repo.path(), "printf changed > source.txt", &[], 1024).execute();
        let GateOutcome::SubjectMutated(mutation) = outcome else {
            panic!("expected source mutation");
        };
        assert_eq!(mutation.changed_paths, vec![PathBuf::from("source.txt")]);
        assert_eq!(
            std::fs::read_to_string(repo.path().join("source.txt")).unwrap(),
            "one\n"
        );

        std::fs::write(repo.path().join(".gitignore"), "target/\n").unwrap();
        let outcome = prepared_gate(
            repo.path(),
            "mkdir -p target; printf generated > target/out",
            &[],
            1024,
        )
        .execute();
        assert!(matches!(outcome, GateOutcome::Passed(_)));
        assert!(!repo.path().join("target").exists());
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_the_process_group() {
        let repo = repository();
        let pid_file = tempfile::NamedTempFile::new().unwrap();
        let path = pid_file.path().display().to_string();
        let mut prepared = prepared_gate(
            repo.path(),
            "sleep 30 & printf '%s' \"$!\" > \"$1\"; wait",
            std::slice::from_ref(&path),
            1024,
        );
        // Leave enough startup headroom when the full suite runs hundreds of
        // tests concurrently; the descendant writes its PID before waiting.
        prepared.gate.spec.timeout = Duration::from_secs(3);
        assert!(matches!(prepared.execute(), GateOutcome::TimedOut(_)));

        let pid: i32 = std::fs::read_to_string(path).unwrap().parse().unwrap();
        let gone = (0..100).any(|_| {
            // SAFETY: signal 0 probes liveness without sending a signal.
            let result = unsafe { libc::kill(pid, 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                true
            } else {
                std::thread::sleep(Duration::from_millis(10));
                false
            }
        });
        assert!(gone, "gate descendant {pid} survived timeout cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn passing_receipt_is_content_addressed_and_persisted_immutably() {
        let repo = repository();
        let (receipt, resolved, subject) = receipt(repo.path());

        assert!(receipt.validate());
        assert_eq!(
            receipt_applicability(&receipt, subject.subject(), &resolved),
            ReceiptApplicability::Current
        );
        let path = persist_receipt(repo.path(), &receipt).unwrap();
        assert_eq!(persist_receipt(repo.path(), &receipt).unwrap(), path);
        assert_eq!(read_receipts(repo.path()).unwrap(), vec![receipt]);
    }

    #[cfg(unix)]
    #[test]
    fn applicability_distinguishes_tree_point_definition_and_repository() {
        let repo = repository();
        let (receipt, resolved, _) = receipt(repo.path());

        std::fs::write(repo.path().join("source.txt"), "two\n").unwrap();
        let dirty = verify_subject(repo.path()).unwrap();
        assert_eq!(
            receipt_applicability(&receipt, dirty.subject(), &resolved),
            ReceiptApplicability::StaleTree
        );

        std::fs::write(repo.path().join("source.txt"), "one\n").unwrap();
        write_gate(repo.path(), "printf ok");
        let unstaged = verify_subject(repo.path()).unwrap();
        git(repo.path(), &["add", "gate.sh"]);
        let staged = verify_subject(repo.path()).unwrap();
        assert_eq!(
            unstaged.subject.tree_oid, staged.subject.tree_oid,
            "staging the same bytes must keep the source tree"
        );
        assert_eq!(
            receipt_applicability(&receipt, staged.subject(), &resolved),
            ReceiptApplicability::StaleRepositoryPoint
        );

        let changed = catalog(&[("check", config("./gate.sh", &["different"]))], &[])
            .unwrap()
            .resolve(&GateId::parse("check").unwrap())
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            receipt_applicability(&receipt, receipt.subject(), &changed),
            ReceiptApplicability::GateDefinitionChanged
        );

        let mut other = receipt.subject().clone();
        other.repository = RepositoryRef::parse("repo_other").unwrap();
        assert_eq!(
            receipt_applicability(&receipt, &other, &resolved),
            ReceiptApplicability::RepositoryMismatch
        );
    }

    #[cfg(unix)]
    #[test]
    fn tampered_receipt_is_invalid_not_current() {
        let repo = repository();
        let (receipt, resolved, subject) = receipt(repo.path());
        let mut value = serde_json::to_value(receipt).unwrap();
        value["stdout"]["tail"] = serde_json::Value::String("forged".into());
        let forged: GateReceipt = serde_json::from_value(value).unwrap();
        assert_eq!(
            receipt_applicability(&forged, subject.subject(), &resolved),
            ReceiptApplicability::InvalidReceipt
        );
    }
}
