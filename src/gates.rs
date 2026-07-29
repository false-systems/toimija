//! Trusted repository gate definitions.
//!
//! Gate requirements carry only [`GateId`]s. This module is the only place
//! those IDs become executable plus argv, and it never parses a shell string.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateWorkingDirectoryConfig {
    RepositoryRoot,
    Relative(PathBuf),
}

impl Default for GateWorkingDirectoryConfig {
    fn default() -> Self {
        Self::RepositoryRoot
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceMutationPolicy {
    Forbid,
}

impl Default for SourceMutationPolicy {
    fn default() -> Self {
        Self::Forbid
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    Deny,
    Allow,
    UnenforcedDenyRequested,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self::UnenforcedDenyRequested
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateEvidenceGrade {
    IsolatedSnapshot,
}

impl Default for GateEvidenceGrade {
    fn default() -> Self {
        Self::IsolatedSnapshot
    }
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
    repo_root: PathBuf,
    lease_id: String,
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
        let result = materialize_worktree(&runner, subject, &root)
            .and_then(|()| validate_workspace_symlinks(&root));
        if let Err(error) = result {
            let _ = remove_worktree(&runner, subject.repo_root(), &root);
            let _ = fs::remove_dir_all(&root);
            let _ = crate::snapshot::release_lease(&runner, subject.repo_root(), &lease_id);
            return Err(error);
        }
        Ok(Self {
            root,
            repo_root: subject.repo_root.clone(),
            lease_id,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn source_tree(&self) -> Result<String> {
        crate::snapshot::materialize(&crate::command::ProcessCommandRunner, &self.root)
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
        let _ = remove_worktree(&runner, &self.repo_root, &self.root);
        let _ = fs::remove_dir_all(&self.root);
        let _ = crate::snapshot::release_lease(&runner, &self.repo_root, &self.lease_id);
    }
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

fn materialize_worktree(
    runner: &impl CommandRunner,
    subject: &VerifiedGateSubject,
    root: &Path,
) -> Result<()> {
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
    let added = runner
        .run(
            subject.repo_root(),
            "git",
            &["worktree", "add", "--detach", root_text, commit],
        )
        .context("failed to materialize the gate workspace")?;
    if added.status != 0 {
        bail!(
            "failed to materialize the gate workspace: {}",
            added.stderr_lossy().trim()
        );
    }
    Ok(())
}

fn remove_worktree(runner: &impl CommandRunner, repo_root: &Path, root: &Path) -> Result<()> {
    let root = root
        .to_str()
        .context("temporary gate workspace path is not UTF-8")?;
    let output = runner.run(repo_root, "git", &["worktree", "remove", "--force", root])?;
    if output.status != 0 {
        bail!(
            "failed to remove gate workspace: {}",
            output.stderr_lossy().trim()
        );
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
    fn workspace_rejects_symlinks_that_escape_the_snapshot() {
        use std::os::unix::fs::symlink;

        let repo = repository();
        symlink("/tmp", repo.path().join("escape")).unwrap();
        git(repo.path(), &["add", "escape"]);
        let subject = verify_subject(repo.path()).unwrap();
        let error = GateWorkspace::materialize(&subject).err().unwrap();
        assert!(error.to_string().contains("escapes the snapshot"));
    }
}
