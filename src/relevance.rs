//! Git-derived relevance.
//!
//! Toimija reports Git *state*; this layer mines bounded Git *history* to help
//! an agent decide what to read first and what to ignore. It is advisory and
//! pure Git — `git status`/`diff`/`log` (and `blame` only when cheap). It does
//! not parse source, talk to a network, or build an index. Every hint carries
//! an honest [`RelevanceSource`] so the reader knows it is heuristic, not
//! semantic truth.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The relevance hints attached to a context packet. Every field is empty when
/// the underlying Git data is unavailable (shallow clone, no history, …), so an
/// absent section always means "could not derive", never "nothing to see".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelevancePacket {
    pub scope: Option<ScopeHints>,
    pub read_order: Vec<ReadOrderEntry>,
    pub coupling: Vec<CouplingHint>,
    pub risk: Vec<RiskHint>,
    pub warnings: Vec<String>,
}

impl RelevancePacket {
    /// True when there is nothing to render — used to keep the packet quiet
    /// rather than emit empty headings.
    pub fn is_empty(&self) -> bool {
        self.scope.is_none()
            && self.read_order.is_empty()
            && self.coupling.is_empty()
            && self.risk.is_empty()
            && self.warnings.is_empty()
    }
}

/// What the branch is touching versus the areas it leaves alone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeHints {
    pub in_play: Vec<ScopeArea>,
    pub out_of_scope: Vec<ScopeArea>,
    pub source: RelevanceSource,
}

/// A top-level area (directory or notable file group) with the paths that put
/// it in play and a short, evidence-only reason.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeArea {
    pub label: String,
    pub paths: Vec<PathBuf>,
    pub reason: String,
}

/// One ranked group of paths to read, lower `priority` first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadOrderEntry {
    pub paths: Vec<PathBuf>,
    pub reason: String,
    pub priority: u8,
    pub source: RelevanceSource,
}

/// Two files that have historically changed together within the mined window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CouplingHint {
    pub left: PathBuf,
    pub right: PathBuf,
    pub count: usize,
    pub window: String,
    pub source: RelevanceSource,
}

/// A file flagged as risky by a cheap, bounded Git-history heuristic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RiskHint {
    pub path: PathBuf,
    pub kind: RiskKind,
    pub reason: String,
    pub source: RelevanceSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RiskKind {
    HighChurn,
    YoungFile,
    OldLoadBearing,
}

/// Where a hint came from. Kept honest so an agent can weigh it: `GitLog`/
/// `GitBlame`/`MergeBaseDiff` are real Git evidence; `Heuristic` is a
/// file-pattern guess; `Unavailable` means the data could not be derived.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelevanceSource {
    MergeBaseDiff,
    WorkingTreeAndIndex,
    GitLog,
    GitBlame,
    Heuristic,
    Unavailable,
}
