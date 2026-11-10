//! Collection coverage — the difference between "we looked and found nothing"
//! and "we could not look".
//!
//! A [`Coverage`] value is attached at a stage boundary (branch diff, session
//! scan — later stages such as repository projects,
//! instructions, and ownership/CI adopt the same type as they are built). It is
//! deliberately not a generic wrapper around every value: one observation per
//! stage, next to that stage's result. Empty evidence with `available` coverage
//! is a successful empty observation; empty evidence with `failed` coverage is
//! a blind spot, and no claim — positive or negative — may be derived from it.
//!
//! Validation discovery already carries its provenance through
//! [`crate::validate::ValidationSource`]; it does not duplicate it here.

use serde::{Deserialize, Serialize};

/// How completely one collection stage observed its target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Coverage {
    pub status: CoverageStatus,
    /// Safe, human-readable diagnostic when the stage is not fully available.
    /// Never raw stderr — a short statement of what could not be observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What produced (or would have produced) the observation, e.g.
    /// `git diff --name-status` or `session marker scan`.
    pub source: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// The stage ran; its (possibly empty) result is a real observation.
    Available,
    /// The stage was partial or skipped for a stated reason; absence of
    /// evidence is not evidence of absence.
    Degraded,
    /// The stage failed; its result is a blind spot, not an observation.
    Failed,
}

impl Coverage {
    pub fn available(source: impl Into<String>) -> Self {
        Coverage {
            status: CoverageStatus::Available,
            reason: None,
            source: source.into(),
        }
    }

    pub fn degraded(source: impl Into<String>, reason: impl Into<String>) -> Self {
        Coverage {
            status: CoverageStatus::Degraded,
            reason: Some(reason.into()),
            source: source.into(),
        }
    }

    pub fn failed(source: impl Into<String>, reason: impl Into<String>) -> Self {
        Coverage {
            status: CoverageStatus::Failed,
            reason: Some(reason.into()),
            source: source.into(),
        }
    }

    /// Serde default for packets written before coverage existed: honestly
    /// degraded, never retroactively promoted to a real observation.
    pub fn unrecorded() -> Self {
        Coverage::degraded("unrecorded", "packet predates coverage reporting")
    }

    pub fn is_available(&self) -> bool {
        self.status == CoverageStatus::Available
    }
}

impl std::fmt::Display for CoverageStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            CoverageStatus::Available => "available",
            CoverageStatus::Degraded => "degraded",
            CoverageStatus::Failed => "failed",
        };
        formatter.write_str(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_serialize_snake_case() {
        let coverage = Coverage::failed("git diff", "spawn failed");
        let json = serde_json::to_value(&coverage).unwrap();
        assert_eq!(json["status"], "failed");
        assert_eq!(json["reason"], "spawn failed");
        assert_eq!(json["source"], "git diff");
    }

    #[test]
    fn available_omits_reason() {
        let json = serde_json::to_value(Coverage::available("scan")).unwrap();
        assert!(json.get("reason").is_none());
    }
}
