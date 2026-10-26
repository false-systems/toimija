use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::adapters::LanguageAdapter;
use crate::command::CommandRunner;
use crate::config::RepoConfig;
use crate::consistency::Consistency;
use crate::git::{self, ChangedFile, GitState};
use crate::repo::{self, RepoInfo, RepoKind};
use crate::sauma::{self, SaumaEnvelope, SaumaProvenance};
use crate::validate::ValidationPlan;

pub const CONTEXT_SCHEMA: &str = "toimija-context.v2";

fn default_context_schema() -> String {
    CONTEXT_SCHEMA.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextPacket {
    #[serde(default = "default_context_schema")]
    pub schema: String,
    /// Identifies the two files produced by one persisted refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    pub repo: RepoInfo,
    pub repo_kind: RepoKind,
    #[serde(default)]
    pub secondary_languages: Vec<repo::DetectedLanguage>,
    /// Refresh key for the collected, packet-visible Git state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub git: GitState,
    #[serde(with = "crate::path_codec::vec")]
    pub changed_surface: Vec<PathBuf>,
    pub changed_surface_source: ChangedSurfaceSource,
    pub read_first: Vec<ReadFirstEntry>,
    #[serde(with = "crate::path_codec::vec")]
    pub safe_to_ignore: Vec<PathBuf>,
    pub validation_plan: ValidationPlan,
    pub warnings: Vec<String>,
    pub for_diff: bool,
    pub default_budget: Option<usize>,
    /// Verifiable fingerprint of the Git state this packet was compiled from. A
    /// reader (e.g. the harness) checks it against the live repo before trusting
    /// the packet, instead of re-deriving repo state itself.
    pub consistency: Consistency,
    /// Session identity on a per-session packet (issue #84): the wrapped
    /// agent's declared intent and scope, read from the session marker at
    /// refresh time — so `run --intent` reaches the harness, and a mid-session
    /// `toimija intent` update propagates on the next refresh. `None` on
    /// repo-level packets. Additive per the packet contract's evolution rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionBlock>,
    /// Sauma attestation envelope for the serialized packet. Current producers
    /// emit v2; the option remains only so old documents can deserialize before
    /// the verification boundary rejects unattested packets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sauma: Option<SaumaEnvelope>,
}

/// The session half of a per-session packet: who this packet is for and why
/// that session says it is here. `scope` is the agent's declaration, distinct
/// from the Git-derived `changed_surface`. Advisory, like the marker it
/// mirrors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionBlock {
    pub id: String,
    pub intent: Option<String>,
    pub scope: Vec<String>,
    /// Ephemeral execution context: the session's repository point and changed surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<false_agent_protocol::context::ExecutionContextEnvelope>,
    /// Branch and HEAD captured when `run` entered the session. They stay fixed
    /// so later packets can report drift instead of overwriting the baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_head: Option<String>,
}

/// Where the changed surface came from. Named honestly: the surface either
/// reflects the whole branch (diff since the merge-base) or only the local
/// working tree and index, and we never claim branch-diff evidence we lack.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangedSurfaceSource {
    /// A merge-base exists, so the surface includes branch work since the base.
    BranchDiffSinceMergeBase,
    /// No merge-base; the surface reflects only the working tree and index.
    WorkingTreeAndIndex,
    /// No HEAD and no local changes: there is nothing to derive a source from.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadFirstEntry {
    #[serde(with = "crate::path_codec::one")]
    pub path: PathBuf,
    pub reason: String,
}

impl ContextPacket {
    pub fn config_default_budget(&self) -> Option<usize> {
        self.default_budget
    }

    pub fn attach_sauma(&mut self, run: Option<String>) -> serde_json::Result<()> {
        let provenance = self
            .git
            .head_oid
            .iter()
            .cloned()
            .map(|oid| SaumaProvenance::Git { oid })
            .collect();
        let token = self.consistency.token.clone();
        sauma::attach(self, CONTEXT_SCHEMA, provenance, &token, run)
    }
}

impl sauma::Carrier for ContextPacket {
    fn sauma_mut(&mut self) -> &mut Option<SaumaEnvelope> {
        &mut self.sauma
    }
}

pub const IDENTITY_SCHEMA: &str = "toimija-identity.v1";

/// What a repository says it is: the packet's `repo` object, from
/// `.toimija/repo.yaml` alone, with no Git state collected. For a tool that
/// asks only that (ohjaa's system view): `context --identity --json`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Identity {
    pub schema: String,
    pub repo: RepoInfo,
}

/// The identity of the repository at `repo_root`, exactly as the packet
/// compiled there would carry it in `repo`: the same configuration, loaded the
/// same way (a broken repo.yaml fails here as it fails the packet), and the
/// same defaults when there is none.
pub fn identity(repo_root: &Path) -> anyhow::Result<Identity> {
    let config =
        RepoConfig::load_optional(repo_root)?.unwrap_or_else(|| RepoConfig::default_for(repo_root));
    Ok(Identity {
        schema: IDENTITY_SCHEMA.to_string(),
        repo: RepoInfo::from_config(&config, repo_root),
    })
}

/// Discover the worktree at `cwd`, collect its Git state, and compile the
/// packet from it.
pub fn build_context_packet(
    runner: &impl CommandRunner,
    cwd: &Path,
    for_diff: bool,
) -> anyhow::Result<ContextPacket> {
    let worktree = git::discover(runner, cwd)?;
    compile_packet(&git::collect(runner, &worktree)?, for_diff)
}

/// Compile the packet from one collection of the Git state. The
/// configuration, the repo kind, what to read first and the validation plan
/// are read from the worktree now; no git process is started.
pub fn compile_packet(collected: &git::Collected, for_diff: bool) -> anyhow::Result<ContextPacket> {
    let mut git = collected.state.clone();
    let mut warnings = collected.warnings.clone();
    normalize_git_state(&mut git);
    let config = match RepoConfig::load_optional(&git.repo_root)? {
        Some(config) => config,
        None => {
            warnings.push("Missing repo.yaml; run `toimija init`.".to_string());
            RepoConfig::default_for(&git.repo_root)
        }
    };
    let repo_kind = repo::detect_repo_kind(&git.repo_root);
    let safe_to_ignore = stable_paths(config.context.safe_to_ignore.clone());
    let ignore_rules = effective_ignore_rules(&safe_to_ignore);
    let secondary_scan = repo::detect_secondary_langs(&git.repo_root, repo_kind, |path| {
        matches_safe_to_ignore(path, &ignore_rules)
    });
    if secondary_scan.truncated {
        warnings.push("Secondary-language scan stopped at its 4096-entry limit.".to_string());
    } else if secondary_scan.incomplete {
        warnings
            .push("Secondary-language scan was incomplete due to unreadable paths.".to_string());
    }
    if repo_kind == RepoKind::Generic {
        warnings.push("Repo kind is generic; no recognized root manifest was found.".to_string());
    }
    if config.repo.role.trim().is_empty() {
        warnings.push("Missing product role in .toimija/repo.yaml.".to_string());
    }
    if config.boundaries.is_empty() {
        warnings.push("Missing product boundaries in .toimija/repo.yaml.".to_string());
    }
    if matches!(repo_kind, RepoKind::Rust) && git.repo_root.join("mix.exs").exists() {
        warnings.push(
            "Both Rust and Elixir markers detected; Rust chosen deterministically.".to_string(),
        );
    }
    let adapter = crate::adapters::for_kind(repo_kind);
    let mut validation_plan = adapter.validation_plan(&git.repo_root, &config);
    for note in &validation_plan.notes {
        if note.contains("inferred") {
            warnings.push(note.clone());
        }
    }
    let refresh_token = refresh_token(&git);
    // Effective ignore set = the repo's configured rules plus a small built-in
    // set of build/vendor dirs. Applied live (derived each run), not seeded into
    // repo.yaml, so it tracks the repo as it changes and never drifts. The
    // rendered `safe_to_ignore` field stays the user's configured rules only.
    let changed = filter_ignored(all_changed(&git), &ignore_rules);
    let changed_surface_source = changed_surface_source(&git);
    debug!(
        event = "toimija.git.changed_surface.selected",
        source = ?changed_surface_source,
        files = changed.len()
    );
    let changed_surface: Vec<PathBuf> = changed.iter().map(|file| file.path.clone()).collect();
    // Enrich the plan with a focused tier now that the changed surface is known.
    // The base plan (config- or kind-derived) is whole-repo; this adds advisory
    // commands scoped to what actually changed, rendered above the Fast tier.
    // A declared gate stands alone: it is what to run, and nothing guessed is
    // put beside it.
    if validation_plan.gate.is_none() {
        validation_plan.focused = adapter.focused_validation(&git.repo_root, &changed_surface);
    }
    let read_first = read_first(
        &git.repo_root,
        &config,
        adapter.as_ref(),
        &changed,
        &mut warnings,
    );
    warnings = stable_strings(warnings);
    // Fingerprint the Git state this packet is compiled from, before it moves
    // into the packet, so a reader can verify the packet against the live repo.
    let consistency = Consistency::fingerprint(&git);
    let packet = ContextPacket {
        schema: CONTEXT_SCHEMA.to_string(),
        artifact_id: None,
        repo: RepoInfo::from_config(&config, &git.repo_root),
        repo_kind,
        secondary_languages: secondary_scan.languages,
        refresh_token: Some(refresh_token),
        git,
        changed_surface,
        changed_surface_source,
        read_first,
        safe_to_ignore,
        validation_plan,
        warnings,
        for_diff,
        default_budget: Some(config.context.default_budget),
        consistency,
        // Session identity is injected by the per-session refresh writer
        // (`refresh::refresh_session`), which knows the session directory;
        // packet compilation itself is session-agnostic.
        session: None,
        sauma: None,
    };
    debug!(
        event = "toimija.context.git_section.rendered",
        branch = packet.git.branch.as_deref().unwrap_or("none"),
        detached = packet.git.detached
    );
    info!(
        event = "toimija.context.rendered",
        repo_kind = ?packet.repo_kind,
        warnings = packet.warnings.len()
    );
    Ok(packet)
}

/// Version key over the packet-visible repository state, including the Git-ref
/// surface (base, upstream, merge-base, branch files, recent commits) that the
/// consistency token deliberately omits. Repository state only: anything read
/// from the process environment (PATH, HOME, config files outside the repo) is
/// banned from this input, or two honest observers of the same repo would
/// compute different versions and staleness would never converge
/// (proposal 0011).
pub(crate) fn refresh_token(git: &GitState) -> String {
    let mut git = git.clone();
    git.staged_files
        .retain(|file| !crate::consistency::is_generated_output(&file.path));
    git.unstaged_files
        .retain(|file| !crate::consistency::is_generated_output(&file.path));
    git.untracked_files
        .retain(|path| !crate::consistency::is_generated_output(path));
    git.conflicted_files
        .retain(|path| !crate::consistency::is_generated_output(path));
    git.conflict_entries
        .retain(|entry| !crate::consistency::is_generated_output(&entry.path));
    git.dirty = git::has_work(
        &git.staged_files,
        &git.unstaged_files,
        &git.untracked_files,
        &git.conflicted_files,
    );
    let state = serde_json::to_vec(&git).expect("refresh state serializes");
    crate::consistency::hash_bytes(&state)
}

/// Decide how to describe the changed surface. A merge-base means we can speak
/// to the branch surface; otherwise we only have the working tree and index;
/// with neither HEAD nor local changes there is nothing to source from.
fn changed_surface_source(git: &GitState) -> ChangedSurfaceSource {
    let has_local_changes = !git.staged_files.is_empty()
        || !git.unstaged_files.is_empty()
        || !git.untracked_files.is_empty()
        || !git.branch_files.is_empty();
    // The surface reflects the branch diff only when commits since the merge-base
    // actually contributed files. A merge-base that equals HEAD (branch sitting
    // at its base) with only working-tree dirt is working-tree, not branch diff.
    if !git.branch_files.is_empty() {
        ChangedSurfaceSource::BranchDiffSinceMergeBase
    } else if git.head_oid.is_some() || has_local_changes {
        ChangedSurfaceSource::WorkingTreeAndIndex
    } else {
        ChangedSurfaceSource::Unknown
    }
}

fn all_changed(git: &GitState) -> Vec<ChangedFile> {
    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    for file in git
        .staged_files
        .iter()
        .chain(git.unstaged_files.iter())
        .chain(git.branch_files.iter())
        .cloned()
        .chain(git.untracked_files.iter().cloned().map(|path| ChangedFile {
            path,
            status: git::ChangeStatus::Untracked,
            index_entry: None,
            original_path: None,
            similarity: None,
        }))
    {
        if seen.insert(file.path.clone()) {
            files.push(file);
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
}

fn filter_ignored(files: Vec<ChangedFile>, safe_to_ignore: &[PathBuf]) -> Vec<ChangedFile> {
    files
        .into_iter()
        .filter(|file| !matches_safe_to_ignore(&file.path, safe_to_ignore))
        .collect()
}

pub(crate) fn normalize_git_state(git: &mut GitState) {
    sort_changed_files(&mut git.staged_files);
    sort_changed_files(&mut git.unstaged_files);
    sort_changed_files(&mut git.branch_files);
    git.untracked_files = stable_paths(std::mem::take(&mut git.untracked_files));
    git.conflicted_files = stable_paths(std::mem::take(&mut git.conflicted_files));
    git.conflict_entries
        .sort_by(|left, right| left.path.cmp(&right.path));
    git.conflict_entries
        .dedup_by(|left, right| left.path == right.path);
}

fn sort_changed_files(files: &mut Vec<ChangedFile>) {
    files.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(status_rank(&left.status).cmp(&status_rank(&right.status)))
    });
    files.dedup_by(|left, right| left.path == right.path && left.status == right.status);
}

fn status_rank(status: &git::ChangeStatus) -> u8 {
    match status {
        git::ChangeStatus::Added => 0,
        git::ChangeStatus::Modified => 1,
        git::ChangeStatus::Deleted => 2,
        git::ChangeStatus::Renamed => 3,
        git::ChangeStatus::Copied => 4,
        git::ChangeStatus::Untracked => 5,
        git::ChangeStatus::Unknown(_) => 6,
    }
}

fn stable_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Build/vendor directories Toimija always treats as noise, matched as a path
/// segment anywhere in the tree. Kept small and near-universal — these are build
/// output or vendored deps, not product source. Applied live every run.
const BUILTIN_ARTIFACT_DIRS: &[&str] = &["target", "_build", "deps", "node_modules", "vendor"];

pub(crate) fn effective_ignore_rules(configured: &[PathBuf]) -> Vec<PathBuf> {
    let mut rules: Vec<PathBuf> = configured.to_vec();
    rules.extend(BUILTIN_ARTIFACT_DIRS.iter().map(PathBuf::from));
    rules
}

pub(crate) fn matches_safe_to_ignore(path: &Path, rules: &[PathBuf]) -> bool {
    let path_segments = normal_segments(path);
    rules
        .iter()
        .any(|rule| rule_matches_segments(&path_segments, rule))
}

/// A rule matches when its directory segments appear as a contiguous run of
/// components anywhere in the path. So `target` matches `a/b/target/x` but not
/// `targeted.rs` or `targets/`, and `deploy/container` matches only a real
/// `deploy/container/...` sequence. Component-wise, never substring.
fn rule_matches_segments(path_segments: &[&std::ffi::OsStr], rule: &Path) -> bool {
    let rule_segments = normal_segments(rule);
    if rule_segments.is_empty() {
        return false;
    }
    path_segments
        .windows(rule_segments.len())
        .any(|window| window == rule_segments.as_slice())
}

/// Normal path components only — drops `/`, `.`, and `..` and any trailing
/// separator, so `./target/` and `target` compare equal.
fn normal_segments(path: &Path) -> Vec<&std::ffi::OsStr> {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(segment) => Some(segment),
            _ => None,
        })
        .collect()
}

fn stable_strings(strings: Vec<String>) -> Vec<String> {
    strings
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// What to read first: the repository's own list (`context.read_first`, then
/// `important_docs`) first, so a wide branch can never push it out of a capped
/// rendering, then the changed files, then their companion tests. A path
/// appears once, under the first reason that names it.
fn read_first(
    repo_root: &Path,
    config: &RepoConfig,
    adapter: &dyn LanguageAdapter,
    changed: &[ChangedFile],
    warnings: &mut Vec<String>,
) -> Vec<ReadFirstEntry> {
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for path in &config.context.read_first {
        if repo_root.join(path).exists() {
            if seen.insert(path.clone()) {
                entries.push(ReadFirstEntry {
                    path: path.clone(),
                    reason: "repo config".to_string(),
                });
            }
        } else {
            warnings.push(format!(
                "Configured read_first path does not exist: {}",
                path.display()
            ));
        }
    }
    for doc in &config.context.important_docs {
        if repo_root.join(doc).exists() && seen.insert(doc.clone()) {
            entries.push(ReadFirstEntry {
                path: doc.clone(),
                reason: "important doc".to_string(),
            });
        }
    }
    for file in changed {
        if seen.insert(file.path.clone()) {
            entries.push(ReadFirstEntry {
                path: file.path.clone(),
                reason: "changed".to_string(),
            });
        }
    }
    let mut hints = adapter.read_first_hints(repo_root, changed);
    hints.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    for (path, reason) in hints {
        if seen.insert(path.clone()) {
            entries.push(ReadFirstEntry { path, reason });
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::generic::GenericAdapter;
    use crate::git::{ChangeStatus, ChangedFile};

    #[test]
    fn refresh_token_covers_git_state_and_ignores_observer_environment() {
        let mut git = GitState {
            repo_root: PathBuf::from("/repo"),
            git_dir: Some(PathBuf::from(".git")),
            branch: Some("feature".to_string()),
            head_oid: Some("abc".to_string()),
            upstream: None,
            ahead: None,
            behind: None,
            ahead_behind_ref: None,
            base: None,
            merge_base: None,
            detached: false,
            dirty: false,
            staged_files: Vec::new(),
            unstaged_files: Vec::new(),
            untracked_files: Vec::new(),
            conflicted_files: Vec::new(),
            conflict_entries: Vec::new(),
            branch_files: Vec::new(),
            branch_diff_coverage: crate::coverage::Coverage::unrecorded(),
            recent_commits: Vec::new(),
        };
        let original = refresh_token(&git);

        // Ref movement is repository state: it must change the token.
        git.base = Some("origin/main".to_string());
        assert_ne!(refresh_token(&git), original);
        git.base = None;

        // The token is a pure function of repository state (proposal 0011):
        // nothing read from the observer's environment is an input to it, so
        // the same repo always yields the same token.
        assert_eq!(refresh_token(&git), original);
    }

    #[test]
    fn context_packet_serializes_to_json() {
        let git = GitState {
            repo_root: PathBuf::from("/repo"),
            git_dir: Some(PathBuf::from(".git")),
            branch: Some("main".to_string()),
            head_oid: Some("abc".to_string()),
            upstream: None,
            ahead: None,
            behind: None,
            ahead_behind_ref: None,
            base: None,
            merge_base: None,
            detached: false,
            dirty: false,
            staged_files: Vec::new(),
            unstaged_files: Vec::new(),
            untracked_files: Vec::new(),
            conflicted_files: Vec::new(),
            conflict_entries: Vec::new(),
            branch_files: Vec::new(),
            branch_diff_coverage: crate::coverage::Coverage::unrecorded(),
            recent_commits: Vec::new(),
        };
        let consistency = Consistency::fingerprint(&git);
        let packet = ContextPacket {
            schema: CONTEXT_SCHEMA.to_string(),
            artifact_id: None,
            repo: RepoInfo {
                name: "repo".to_string(),
                role: None,
                not_roles: Vec::new(),
                important_docs: vec![PathBuf::from("README.md")],
                boundaries: Vec::new(),
            },
            repo_kind: RepoKind::Generic,
            secondary_languages: Vec::new(),
            refresh_token: None,
            git,
            changed_surface: Vec::new(),
            changed_surface_source: ChangedSurfaceSource::Unknown,
            read_first: Vec::new(),
            safe_to_ignore: Vec::new(),
            validation_plan: crate::validate::validation_plan(
                Path::new("/repo"),
                &RepoConfig::default(),
                RepoKind::Generic,
            ),
            warnings: Vec::new(),
            for_diff: false,
            default_budget: Some(4000),
            consistency,
            session: None,
            sauma: None,
        };
        let json = serde_json::to_string(&packet).unwrap();
        assert!(json.contains("\"repo\""));
    }

    #[test]
    fn context_packet_defaults_missing_schema() {
        let git = GitState {
            repo_root: PathBuf::from("/repo"),
            git_dir: Some(PathBuf::from(".git")),
            branch: Some("main".to_string()),
            head_oid: Some("abc".to_string()),
            upstream: None,
            ahead: None,
            behind: None,
            ahead_behind_ref: None,
            base: None,
            merge_base: None,
            detached: false,
            dirty: false,
            staged_files: Vec::new(),
            unstaged_files: Vec::new(),
            untracked_files: Vec::new(),
            conflicted_files: Vec::new(),
            conflict_entries: Vec::new(),
            branch_files: Vec::new(),
            branch_diff_coverage: crate::coverage::Coverage::unrecorded(),
            recent_commits: Vec::new(),
        };
        let packet = ContextPacket {
            schema: CONTEXT_SCHEMA.to_string(),
            artifact_id: None,
            repo: RepoInfo {
                name: "repo".to_string(),
                role: None,
                not_roles: Vec::new(),
                important_docs: Vec::new(),
                boundaries: Vec::new(),
            },
            repo_kind: RepoKind::Generic,
            secondary_languages: Vec::new(),
            refresh_token: None,
            git: git.clone(),
            changed_surface: Vec::new(),
            changed_surface_source: ChangedSurfaceSource::Unknown,
            read_first: Vec::new(),
            safe_to_ignore: Vec::new(),
            validation_plan: crate::validate::validation_plan(
                Path::new("/repo"),
                &RepoConfig::default(),
                RepoKind::Generic,
            ),
            warnings: Vec::new(),
            for_diff: false,
            default_budget: Some(4000),
            consistency: Consistency::fingerprint(&git),
            session: None,
            sauma: None,
        };
        let mut value = serde_json::to_value(packet).unwrap();
        value.as_object_mut().unwrap().remove("schema");
        let decoded: ContextPacket = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.schema, CONTEXT_SCHEMA);
    }

    #[test]
    fn packet_sauma_digest_round_trips_without_self_reference() {
        let git = GitState {
            repo_root: PathBuf::from("/repo"),
            git_dir: Some(PathBuf::from(".git")),
            branch: Some("main".to_string()),
            head_oid: Some("abc".to_string()),
            upstream: None,
            ahead: None,
            behind: None,
            ahead_behind_ref: None,
            base: None,
            merge_base: None,
            detached: false,
            dirty: false,
            staged_files: Vec::new(),
            unstaged_files: Vec::new(),
            untracked_files: Vec::new(),
            conflicted_files: Vec::new(),
            conflict_entries: Vec::new(),
            branch_files: Vec::new(),
            branch_diff_coverage: crate::coverage::Coverage::unrecorded(),
            recent_commits: Vec::new(),
        };
        let consistency = Consistency::fingerprint(&git);
        let mut packet = ContextPacket {
            schema: CONTEXT_SCHEMA.to_string(),
            artifact_id: None,
            repo: RepoInfo {
                name: "repo".to_string(),
                role: None,
                not_roles: Vec::new(),
                important_docs: Vec::new(),
                boundaries: Vec::new(),
            },
            repo_kind: RepoKind::Generic,
            secondary_languages: Vec::new(),
            refresh_token: None,
            git,
            changed_surface: Vec::new(),
            changed_surface_source: ChangedSurfaceSource::Unknown,
            read_first: Vec::new(),
            safe_to_ignore: Vec::new(),
            validation_plan: crate::validate::validation_plan(
                Path::new("/repo"),
                &RepoConfig::default(),
                RepoKind::Generic,
            ),
            warnings: Vec::new(),
            for_diff: false,
            default_budget: Some(4000),
            consistency,
            session: Some(SessionBlock {
                id: "100-1".to_string(),
                intent: None,
                scope: Vec::new(),
                execution: None,
                entry_branch: None,
                entry_head: None,
            }),
            sauma: None,
        };

        packet.attach_sauma(Some("100-1".to_string())).unwrap();
        let envelope = packet.sauma.as_ref().unwrap();
        let digest = envelope.digest.clone();
        let value = serde_json::to_value(&packet).unwrap();

        assert_eq!(crate::sauma::artifact_digest_value(&value), digest);
        assert_eq!(envelope.schema, CONTEXT_SCHEMA);
        assert_eq!(envelope.producer.run.as_deref(), Some("100-1"));
        assert_eq!(
            envelope.provenance,
            vec![SaumaProvenance::Git {
                oid: "abc".to_string()
            }]
        );
    }

    #[test]
    fn read_first_lists_configured_docs_before_changed_files() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("README.md"), "docs").unwrap();
        let mut config = RepoConfig::default();
        config.context.important_docs = vec![PathBuf::from("README.md")];
        let changed = vec![ChangedFile {
            path: PathBuf::from("src/lib.rs"),
            status: ChangeStatus::Modified,
            index_entry: None,
            original_path: None,
            similarity: None,
        }];
        let mut warnings = Vec::new();
        let entries = read_first(
            temp.path(),
            &config,
            &GenericAdapter,
            &changed,
            &mut warnings,
        );
        let paths: Vec<&Path> = entries.iter().map(|entry| entry.path.as_path()).collect();
        assert_eq!(paths, [Path::new("README.md"), Path::new("src/lib.rs")]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn read_first_includes_configured_paths_and_warns_when_missing() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("docs")).unwrap();
        std::fs::write(temp.path().join("docs/architecture.md"), "arch").unwrap();
        let mut config = RepoConfig::default();
        config.context.read_first = vec![
            PathBuf::from("docs/architecture.md"),
            PathBuf::from("docs/missing.md"),
        ];
        let mut warnings = Vec::new();

        let entries = read_first(temp.path(), &config, &GenericAdapter, &[], &mut warnings);

        assert!(entries.iter().any(|entry| {
            entry.path == Path::new("docs/architecture.md") && entry.reason == "repo config"
        }));
        assert_eq!(
            warnings,
            vec!["Configured read_first path does not exist: docs/missing.md"]
        );
    }

    #[test]
    fn safe_to_ignore_filters_changed_paths_by_prefix() {
        let files = vec![
            ChangedFile {
                path: PathBuf::from("src/lib.rs"),
                status: ChangeStatus::Modified,
                index_entry: None,
                original_path: None,
                similarity: None,
            },
            ChangedFile {
                path: PathBuf::from("target/debug/build.log"),
                status: ChangeStatus::Untracked,
                index_entry: None,
                original_path: None,
                similarity: None,
            },
            ChangedFile {
                path: PathBuf::from("_build/dev/app.beam"),
                status: ChangeStatus::Untracked,
                index_entry: None,
                original_path: None,
                similarity: None,
            },
        ];

        let filtered = filter_ignored(files, &[PathBuf::from("target/"), PathBuf::from("_build")]);

        assert_eq!(
            filtered
                .iter()
                .map(|file| file.path.as_path())
                .collect::<Vec<_>>(),
            vec![Path::new("src/lib.rs")]
        );
    }

    #[test]
    fn ignore_matching_is_segment_aware_not_substring() {
        let rules = [PathBuf::from("target/")];
        // Nested anywhere in the tree is ignored.
        assert!(matches_safe_to_ignore(
            Path::new("test_projects/rust_test/target/debug/junk.o"),
            &rules
        ));
        assert!(matches_safe_to_ignore(Path::new("target/x"), &rules));
        // Substring / sibling names are NOT ignored — component match only.
        assert!(!matches_safe_to_ignore(
            Path::new("a/b/targeted.rs"),
            &rules
        ));
        assert!(!matches_safe_to_ignore(Path::new("targets/x"), &rules));
        assert!(!matches_safe_to_ignore(Path::new("src/lib.rs"), &rules));
    }

    #[test]
    fn multi_segment_rules_match_contiguous_sequence() {
        let rules = [PathBuf::from("deploy/container")];
        assert!(matches_safe_to_ignore(
            Path::new("deploy/container/ahti.toml"),
            &rules
        ));
        // Only the full sequence matches; a partial tail does not.
        assert!(!matches_safe_to_ignore(
            Path::new("container/ahti.toml"),
            &rules
        ));
    }

    #[test]
    fn builtin_artifact_dirs_are_filtered_even_with_empty_config() {
        let rules = effective_ignore_rules(&[]);
        // No configured rules, yet built-in build/vendor dirs are ignored live.
        assert!(matches_safe_to_ignore(
            Path::new("test_projects/rust_test/target/x"),
            &rules
        ));
        assert!(matches_safe_to_ignore(
            Path::new("apps/foo/_build/y"),
            &rules
        ));
        assert!(matches_safe_to_ignore(Path::new("deps/grpc/z"), &rules));
        assert!(matches_safe_to_ignore(
            Path::new("web/node_modules/pkg/i.js"),
            &rules
        ));
        assert!(matches_safe_to_ignore(
            Path::new("vendor/pkg/lib.rs"),
            &rules
        ));
        // Real source survives.
        assert!(!matches_safe_to_ignore(Path::new("lib/foo/bar.ex"), &rules));
    }

    #[test]
    fn context_ordering_rules_are_stable() {
        let mut git = GitState {
            repo_root: PathBuf::from("/repo"),
            git_dir: None,
            branch: Some("main".to_string()),
            head_oid: Some("abc".to_string()),
            upstream: None,
            ahead: None,
            behind: None,
            ahead_behind_ref: None,
            base: None,
            merge_base: None,
            detached: false,
            dirty: true,
            staged_files: vec![
                ChangedFile {
                    path: PathBuf::from("src/z.rs"),
                    status: ChangeStatus::Modified,
                    index_entry: None,
                    original_path: None,
                    similarity: None,
                },
                ChangedFile {
                    path: PathBuf::from("src/a.rs"),
                    status: ChangeStatus::Added,
                    index_entry: None,
                    original_path: None,
                    similarity: None,
                },
            ],
            unstaged_files: vec![ChangedFile {
                path: PathBuf::from("src/a.rs"),
                status: ChangeStatus::Modified,
                index_entry: None,
                original_path: None,
                similarity: None,
            }],
            untracked_files: vec![PathBuf::from("notes.md"), PathBuf::from("notes.md")],
            conflicted_files: vec![PathBuf::from("b.txt"), PathBuf::from("a.txt")],
            conflict_entries: Vec::new(),
            branch_files: vec![
                ChangedFile {
                    path: PathBuf::from("src/z.rs"),
                    status: ChangeStatus::Modified,
                    index_entry: None,
                    original_path: None,
                    similarity: None,
                },
                ChangedFile {
                    path: PathBuf::from("src/a.rs"),
                    status: ChangeStatus::Added,
                    index_entry: None,
                    original_path: None,
                    similarity: None,
                },
            ],
            branch_diff_coverage: crate::coverage::Coverage::unrecorded(),
            recent_commits: Vec::new(),
        };

        normalize_git_state(&mut git);
        assert_eq!(
            git.branch_files
                .iter()
                .map(|file| file.path.as_path())
                .collect::<Vec<_>>(),
            vec![Path::new("src/a.rs"), Path::new("src/z.rs")]
        );
        assert_eq!(
            git.staged_files
                .iter()
                .map(|file| file.path.as_path())
                .collect::<Vec<_>>(),
            vec![Path::new("src/a.rs"), Path::new("src/z.rs")]
        );
        assert_eq!(
            git.conflicted_files,
            vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")]
        );

        let changed = all_changed(&git);
        assert_eq!(
            changed
                .iter()
                .map(|file| file.path.as_path())
                .collect::<Vec<_>>(),
            vec![
                Path::new("notes.md"),
                Path::new("src/a.rs"),
                Path::new("src/z.rs")
            ]
        );
        assert_eq!(
            stable_strings(vec!["z".to_string(), "a".to_string(), "a".to_string()]),
            vec!["a".to_string(), "z".to_string()]
        );
    }
}
