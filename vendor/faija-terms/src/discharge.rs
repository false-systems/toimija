//! What it takes to discharge an assurance obligation.
//!
//! An earlier version accepted any two sealed documents as discharging any two
//! obligations: citation was mistaken for establishment. This says, per
//! obligation, what *kind* of artifact can discharge it and what structure that
//! artifact must have.
//!
//! # The ceiling, stated once
//!
//! No machine checks that a root cause is *correct*, that an invariant is *the
//! right* invariant, or that a consumer-impact analysis is *complete*. What is
//! checkable is **structural adequacy**: an artifact of the right kind, bound to
//! the right work, containing the internal structure the obligation requires.
//!
//! Structural adequacy is **necessary, not sufficient to establish correctness**. A
//! verdict must therefore report which predicates were *checked* and which were
//! merely *declared*, so nobody reads "structurally adequate" as "true".

use serde::{Deserialize, Serialize};

use crate::knowledge::AssuranceObligation;

/// A structural property of an artifact offered against an obligation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Predicate {
    /// The artifact names the work it concerns, and it is this work.
    BindsToWork,
    /// The artifact is anchored at the point being closed.
    AnchoredAtFinalPoint,
    /// An investigation containing a finding with status `supported`.
    CarriesSupportedFinding,
    /// A `caused_by` edge from a supported finding — the difference between an
    /// observation and a root cause.
    CarriesCausalEdge,
    /// An invariant claim with status `supported`.
    CarriesSupportedInvariant,
    /// A regression witness naming the finding it witnesses.
    NamesTheFindingWitnessed,
    /// A regression witness naming the change it was verified against.
    NamesTheChangeVerified,
    /// Execution evidence whose outcome was observed, not reused from cache.
    OutcomeWasObserved,
    /// Evidence produced by at least two independent implementations.
    CitesIndependentImplementations,
    /// The analysis reaches a stated conclusion rather than recording an intent.
    /// **Not checkable**: whether an analysis is correct or complete is
    /// judgement, and a machine that claimed otherwise would be lying.
    ReachesAConclusion,
    /// The adversarial attempt actually attempted something.
    /// **Not checkable** for the same reason.
    AttemptedABypass,
}

impl Predicate {
    /// Whether a judge can decide this from the artifact's structure alone.
    ///
    /// The unchecked ones are not oversights. They are the boundary where
    /// structure stops and judgement begins, and naming them is what keeps a
    /// verdict honest about its own depth.
    pub fn is_structurally_checkable(self) -> bool {
        !matches!(self, Self::ReachesAConclusion | Self::AttemptedABypass)
    }
}

/// What discharges one obligation.
// Serialize only: the static slices describe a compiled-in rule, so there is
// nothing to deserialize into. A consumer reads the rule from its own build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DischargeRequirement {
    /// Artifact schemas that can discharge this obligation. An artifact of
    /// another kind does not discharge it however well-formed it is.
    pub schemas: &'static [&'static str],
    /// Structure the artifact must have.
    pub predicates: &'static [Predicate],
}

impl DischargeRequirement {
    /// Predicates a judge must evaluate.
    pub fn checkable(&self) -> impl Iterator<Item = Predicate> + '_ {
        self.predicates
            .iter()
            .copied()
            .filter(|predicate| predicate.is_structurally_checkable())
    }

    /// Predicates that remain human judgement.
    pub fn unchecked(&self) -> impl Iterator<Item = Predicate> + '_ {
        self.predicates
            .iter()
            .copied()
            .filter(|predicate| !predicate.is_structurally_checkable())
    }
}

impl AssuranceObligation {
    /// The artifact kind and structure that discharges this obligation.
    pub fn discharge_requirement(self) -> DischargeRequirement {
        use Predicate::*;
        match self {
            // Answering the question is the only thing that answers it, so no
            // artifact discharges classification.
            Self::WorkClassified => DischargeRequirement {
                schemas: &[],
                predicates: &[],
            },
            Self::RootCauseEstablished => DischargeRequirement {
                schemas: &["faija-investigation.v1"],
                predicates: &[BindsToWork, CarriesSupportedFinding, CarriesCausalEdge],
            },
            Self::InvariantDefined => DischargeRequirement {
                schemas: &["faija-investigation.v1"],
                predicates: &[BindsToWork, CarriesSupportedInvariant],
            },
            Self::RelatedSurfaceSearch => DischargeRequirement {
                schemas: &["faija-investigation.v1", "kelpo-analysis.v1"],
                predicates: &[BindsToWork, ReachesAConclusion],
            },
            Self::RegressionWitnessed => DischargeRequirement {
                schemas: &["faija-regression-witness.v1"],
                predicates: &[
                    BindsToWork,
                    NamesTheFindingWitnessed,
                    NamesTheChangeVerified,
                ],
            },
            Self::Verification => DischargeRequirement {
                schemas: &["kelpo-evidence.v1"],
                predicates: &[BindsToWork, AnchoredAtFinalPoint, OutcomeWasObserved],
            },
            Self::AdversarialVerification => DischargeRequirement {
                schemas: &["kelpo-evidence.v1", "kelpo-analysis.v1"],
                predicates: &[BindsToWork, AttemptedABypass],
            },
            Self::ConsumerImpactAnalysis
            | Self::HistoricalCompatibilityAnalysis
            | Self::AuthorityBoundaryReviewed
            | Self::FailureModelReviewed => DischargeRequirement {
                schemas: &["kelpo-analysis.v1"],
                predicates: &[BindsToWork, ReachesAConclusion],
            },
            Self::CrossImplementationConformance => DischargeRequirement {
                schemas: &["kelpo-evidence.v1", "kelpo-analysis.v1"],
                predicates: &[
                    BindsToWork,
                    AnchoredAtFinalPoint,
                    CitesIndependentImplementations,
                ],
            },
        }
    }
}

/// How deeply a discharge was established.
///
/// Recorded per obligation so that adequacy never has to mean one thing for
/// a red→green receipt and another for a prose analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DischargeDepth {
    /// An artifact of the right kind was cited and nothing else was verified.
    /// Every predicate for this obligation is human judgement.
    Declared,
    /// Every structurally checkable predicate held.
    StructurallyChecked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_is_discharged_by_no_artifact() {
        let requirement = AssuranceObligation::WorkClassified.discharge_requirement();
        assert!(requirement.schemas.is_empty());
        assert_eq!(requirement.checkable().count(), 0);
    }

    #[test]
    fn a_root_cause_needs_a_causal_edge_not_just_a_finding() {
        let requirement = AssuranceObligation::RootCauseEstablished.discharge_requirement();
        let checkable: Vec<_> = requirement.checkable().collect();
        assert!(checkable.contains(&Predicate::CarriesSupportedFinding));
        assert!(checkable.contains(&Predicate::CarriesCausalEdge));
    }

    /// A regression witness is not a test: it names the finding and the change.
    #[test]
    fn a_regression_witness_must_name_what_it_witnesses() {
        let checkable: Vec<_> = AssuranceObligation::RegressionWitnessed
            .discharge_requirement()
            .checkable()
            .collect();
        assert!(checkable.contains(&Predicate::NamesTheFindingWitnessed));
        assert!(checkable.contains(&Predicate::NamesTheChangeVerified));
    }

    /// The boundary between structure and judgement is explicit, not implied.
    #[test]
    fn analysis_obligations_are_honestly_unchecked() {
        let requirement = AssuranceObligation::ConsumerImpactAnalysis.discharge_requirement();
        assert!(requirement
            .unchecked()
            .any(|p| p == Predicate::ReachesAConclusion));
        assert!(!Predicate::ReachesAConclusion.is_structurally_checkable());
        assert!(!Predicate::AttemptedABypass.is_structurally_checkable());
    }

    /// An artifact of the wrong kind discharges nothing, however well-formed.
    #[test]
    fn every_obligation_but_classification_names_an_artifact_kind() {
        for obligation in [
            AssuranceObligation::RootCauseEstablished,
            AssuranceObligation::InvariantDefined,
            AssuranceObligation::RelatedSurfaceSearch,
            AssuranceObligation::RegressionWitnessed,
            AssuranceObligation::Verification,
            AssuranceObligation::AdversarialVerification,
            AssuranceObligation::ConsumerImpactAnalysis,
            AssuranceObligation::HistoricalCompatibilityAnalysis,
            AssuranceObligation::CrossImplementationConformance,
            AssuranceObligation::AuthorityBoundaryReviewed,
            AssuranceObligation::FailureModelReviewed,
        ] {
            assert!(
                !obligation.discharge_requirement().schemas.is_empty(),
                "{obligation:?} accepts any artifact"
            );
        }
    }

    #[test]
    fn depth_orders_declared_below_checked() {
        assert!(DischargeDepth::Declared < DischargeDepth::StructurallyChecked);
    }
}
