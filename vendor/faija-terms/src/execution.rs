//! Execution binding and separation vocabulary.
//!
//! Two defects motivated this module, both confirmed against the implementation
//! (`docs/producer-binding.md`):
//!
//! * evidence was associated with an execution context by the *consumer*, from a
//!   caller argument, after the artifact existed;
//! * the relation used as evidence of separation was session inequality, and a
//!   worker mints sessions by running a command.
//!
//! The types here exist so the weaker claims cannot be written down as the
//! stronger ones.

use serde::{Deserialize, Serialize};

/// The execution context an executor records **when it produces an artifact**.
///
/// Recorded at production time, never attached afterwards. A consumer that
/// receives an artifact carrying this may validate it; a consumer that receives
/// one without it may not supply one on the artifact's behalf.
///
/// Only semantic fields appear. Process id, command name and host are
/// deliberately absent: they resemble identity without being identity, and
/// including them invites exactly the confusion this module exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionContextRef {
    /// The session that hosted the execution.
    pub session: String,
    /// The repository point the execution concerned.
    pub repository_point: String,
    /// Sealed cycle digest, when the execution ran as part of one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle: Option<String>,
    /// Workflow leg, when the execution ran as part of a cycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leg: Option<String>,
    /// Attempt ordinal within the leg. Presentation only — an attempt's identity
    /// belongs to the executor's journal, not to this counter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// The allocation this execution ran under, as
    /// [`Allocation::spend_key`](crate::allocation::Allocation::spend_key).
    ///
    /// Closes blocker 3: recorded by the executor at production time, so no
    /// later step can attach, swap, or invent one. Its presence means the
    /// execution *claimed* an allocation — never that the allocation verified.
    /// That is a verifier's answer, carried by
    /// [`AllocationState`](crate::allocation::AllocationState).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocation: Option<String>,
}

impl ExecutionContextRef {
    /// A context for an execution that ran outside any cycle.
    ///
    /// `cycle`, `leg` and `attempt` stay absent rather than taking sentinel
    /// values: "no leg" and "leg number zero" are different facts.
    pub fn unbound(session: impl Into<String>, repository_point: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            repository_point: repository_point.into(),
            cycle: None,
            leg: None,
            attempt: None,
            allocation: None,
        }
    }

    /// Whether the execution named an allocation.
    ///
    /// Claiming is not verifying. A worker can write this field.
    pub fn claims_an_allocation(&self) -> bool {
        self.allocation.is_some()
    }

    /// True when the context names a cycle and a leg.
    ///
    /// This says the execution *claimed* a leg. It does not say the leg was
    /// declared by a sealed cycle, and it certainly does not say the leg was
    /// allocated to this worker by anyone. See [`SeparationClaim`].
    pub fn claims_a_leg(&self) -> bool {
        self.cycle.is_some() && self.leg.is_some()
    }
}

/// How an artifact's execution context came to be associated with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "binding", rename_all = "snake_case")]
pub enum EvidenceBinding {
    /// Recorded by the executor at production time.
    Intrinsic(ExecutionContextRef),
    /// The artifact predates intrinsic binding. It may still be read; it may not
    /// support a separation claim, and nothing may attach a context to it now.
    LegacyUnbound,
}

impl EvidenceBinding {
    /// The context, when one is intrinsic. `None` is not an invitation to supply
    /// one — it is the answer.
    pub fn context(&self) -> Option<&ExecutionContextRef> {
        match self {
            Self::Intrinsic(context) => Some(context),
            Self::LegacyUnbound => None,
        }
    }

    /// Whether this binding can support any separation claim at all.
    pub fn can_support_separation(&self) -> bool {
        matches!(self, Self::Intrinsic(_))
    }
}

/// What a separation check actually established. Ordered weakest to strongest.
///
/// The variants exist because the stack spent a release calling the weakest one
/// by the strongest one's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeparationClaim {
    /// Two distinct session identities were supplied. A worker mints sessions by
    /// running a command, so this carries no trust property.
    SessionDisjoint,
    /// The leg named by an execution appears in the cycle document that
    /// execution names, within that document's declared budget.
    ///
    /// An **internal-consistency** property. It catches inconsistency and
    /// accident. It does not catch fabrication: a cycle carries no authority,
    /// because `sauma stamp --producer` is a free string and `verify_artifact`
    /// never checks authorship. It says nothing about who occupies the leg.
    LegDeclared,
    /// The role was allocated by a party other than the claimant.
    /// **Not establishable by the current architecture**: leg identity is
    /// accepted from argv and the composition owner has no privileged channel.
    CompositionAllocated,
    /// The producer is in a trust domain distinct from the claimant's.
    /// **Not establishable by the current architecture.**
    ProducerIndependent,
}

/// Whether *this implementation* can establish a concept the ontology defines.
///
/// Kept separate from [`SeparationClaim`] on purpose: the ontology is the
/// canonical language in which capability is described, not a mirror of current
/// capability. A concept never leaves the ontology because the code cannot yet
/// prove it, and a concept's presence never implies that it can.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "establishment", rename_all = "snake_case")]
pub enum Establishment {
    /// Establishable, by a named mechanism.
    Established { by: &'static str },
    /// Defined, not establishable here. `missing` must name what is absent, so
    /// the gap stays legible instead of becoming folklore.
    Unsupported { missing: &'static str },
}

impl SeparationClaim {
    /// The strongest claim the architecture can currently establish.
    ///
    /// Raise this only when an implementation genuinely delivers the next rung,
    /// never because a diagram or a threat model would read better.
    /// `the_ceiling_agrees_with_per_claim_establishment` stops it drifting away
    /// from [`Self::establishment`].
    pub const ESTABLISHABLE_CEILING: Self = Self::LegDeclared;

    /// What this implementation can do about the claim, and why not, if not.
    pub fn establishment(self) -> Establishment {
        match self {
            Self::SessionDisjoint => Establishment::Established {
                by: "two distinct sealed toimija-context.v1 envelopes",
            },
            Self::LegDeclared => Establishment::Established {
                by: "the execution's leg appears in the cycle document it names",
            },
            // The terms exist (allocation::RoleInstance, allocation::Allocation)
            // and the verification does not. Naming the objects is not the same
            // as delivering the capability.
            Self::CompositionAllocated => Establishment::Unsupported {
                missing: "a verifier that checks a detached faija-allocation-v1 \
signature against a trust root outside the governed repository, refuses a spent \
allocation, and seals the result into the execution context",
            },
            Self::ProducerIndependent => Establishment::Unsupported {
                missing: "a producer in a trust domain distinct from the claimant's; \
allocation bounds who may occupy a role, not who executes it",
            },
        }
    }

    /// Whether this claim is supportable today.
    pub fn is_establishable(self) -> bool {
        matches!(self.establishment(), Establishment::Established { .. })
    }

    /// Never true for anything below [`Self::ProducerIndependent`], which is
    /// itself not establishable. The method exists so that code asking the
    /// question gets `false` rather than a plausible-looking proxy.
    pub fn establishes_producer_independence(self) -> bool {
        self == Self::ProducerIndependent && self.is_establishable()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_receipt_supports_no_separation_and_offers_no_context() {
        let binding = EvidenceBinding::LegacyUnbound;
        assert!(!binding.can_support_separation());
        assert_eq!(binding.context(), None);
    }

    #[test]
    fn an_execution_outside_a_cycle_claims_no_leg() {
        let context = ExecutionContextRef::unbound("1784940186-27064", "rpoint_abc");
        assert!(!context.claims_a_leg());
        assert_eq!(context.cycle, None);
        assert_eq!(context.attempt, None);
    }

    /// The confirmed attack, as a type-level statement: claiming a leg is not
    /// the same as the leg having been allocated.
    #[test]
    fn claiming_a_leg_is_not_composition_allocation() {
        let self_minted = ExecutionContextRef {
            session: "1784940186-27064".into(),
            repository_point: "rpoint_abc".into(),
            cycle: Some("sha256:anything-the-worker-typed".into()),
            leg: Some("qa".into()),
            attempt: Some(1),
            allocation: None,
        };
        assert!(self_minted.claims_a_leg());
        assert!(!SeparationClaim::CompositionAllocated.is_establishable());
        assert!(!SeparationClaim::CompositionAllocated.establishes_producer_independence());
    }

    #[test]
    fn nothing_currently_establishes_producer_independence() {
        for claim in [
            SeparationClaim::SessionDisjoint,
            SeparationClaim::LegDeclared,
            SeparationClaim::CompositionAllocated,
            SeparationClaim::ProducerIndependent,
        ] {
            assert!(
                !claim.establishes_producer_independence(),
                "{claim:?} claimed producer independence"
            );
        }
    }

    /// If this fails, someone raised the ceiling. That is a threat-model change
    /// and needs the evidence to go with it, not a one-line edit.
    #[test]
    fn the_establishable_ceiling_is_leg_declared() {
        assert_eq!(
            SeparationClaim::ESTABLISHABLE_CEILING,
            SeparationClaim::LegDeclared
        );
        assert!(SeparationClaim::SessionDisjoint.is_establishable());
        assert!(SeparationClaim::LegDeclared.is_establishable());
        assert!(!SeparationClaim::CompositionAllocated.is_establishable());
        assert!(!SeparationClaim::ProducerIndependent.is_establishable());
    }

    /// The ceiling and the per-claim answers must not drift apart. If this
    /// fails, one of them was edited without the other.
    #[test]
    fn the_ceiling_agrees_with_per_claim_establishment() {
        let all = [
            SeparationClaim::SessionDisjoint,
            SeparationClaim::LegDeclared,
            SeparationClaim::CompositionAllocated,
            SeparationClaim::ProducerIndependent,
        ];
        let highest = all
            .iter()
            .copied()
            .filter(|claim| claim.is_establishable())
            .max()
            .expect("something is establishable");
        assert_eq!(highest, SeparationClaim::ESTABLISHABLE_CEILING);
    }

    /// An unsupported concept must say what is absent. A bare `Unsupported`
    /// becomes folklore within a release.
    #[test]
    fn every_unsupported_claim_names_what_is_missing() {
        for claim in [
            SeparationClaim::CompositionAllocated,
            SeparationClaim::ProducerIndependent,
        ] {
            match claim.establishment() {
                Establishment::Unsupported { missing } => {
                    assert!(missing.len() > 40, "{claim:?} gives no usable reason");
                }
                Establishment::Established { .. } => {
                    panic!("{claim:?} became establishable without evidence")
                }
            }
        }
    }

    /// Defining RoleInstance and Allocation names the objects blocker 1 and 3
    /// lacked. It does not deliver the verification blocker 2 needs, and the
    /// ontology must keep saying so until that verifier exists.
    #[test]
    fn naming_the_allocation_objects_did_not_raise_the_ceiling() {
        assert!(!SeparationClaim::CompositionAllocated.is_establishable());
        assert_eq!(
            SeparationClaim::ESTABLISHABLE_CEILING,
            SeparationClaim::LegDeclared
        );
    }

    /// Blocker 3 closed: the execution carries the allocation. Blocker 2 not
    /// closed: carrying it is a claim, not a verification.
    #[test]
    fn claiming_an_allocation_is_not_being_allocated() {
        let mut context = ExecutionContextRef::unbound("1784940186-27064", "rpoint_abc");
        assert!(!context.claims_an_allocation());
        context.allocation = Some("sha256:cycle/qa#1@rpoint_abc!n1".into());
        assert!(context.claims_an_allocation());
        assert!(!SeparationClaim::CompositionAllocated.is_establishable());
    }

    #[test]
    fn session_disjointness_is_the_weakest_claim() {
        assert!(SeparationClaim::SessionDisjoint < SeparationClaim::LegDeclared);
        assert!(SeparationClaim::LegDeclared < SeparationClaim::CompositionAllocated);
        assert!(SeparationClaim::CompositionAllocated < SeparationClaim::ProducerIndependent);
    }
}
