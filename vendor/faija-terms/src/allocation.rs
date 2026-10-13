//! Execution-role allocation.
//!
//! Three blockers made `CompositionAllocated` underivable
//! (`docs/authority-chain.md`), each sufficient alone:
//!
//! 1. no role *instance* existed — `cycle.v1` declares role **types**
//!    (`worker_role: String`) and budgets, so allocation had nothing to attach
//!    to, independent of who was trusted;
//! 2. no allocator was distinguishable from a worker — Ohjaa passes plain
//!    flags, cycle authorship is unchecked, `--producer` is a free string;
//! 3. no binding survived into evidence — `cycle_action` is stripped from the
//!    sealed packet and Sykli receipts have no session awareness.
//!
//! This module defines the objects that close 1 and 3, and the *statement* that
//! closes 2. It defines no cryptography: the asymmetry that makes an allocator
//! distinguishable is the detached OpenSSH signature the estate already uses for
//! Kisko judgement, applied to a new statement under its own namespace. Faija
//! says what is signed and what it means. It never signs, verifies, or stores.

use serde::{Deserialize, Serialize};

/// Signature namespace for an allocation statement.
///
/// Distinct from `kisko-decision-v1` on purpose. Reusing Kisko's namespace would
/// let a signature over engineering judgement be replayed as a signature over
/// role allocation — the semantic-inflation failure this whole investigation
/// exists to prevent. A signature authenticates a statement, not every later
/// interpretation of it.
pub const ALLOCATION_NAMESPACE: &str = "faija-allocation-v1";

/// One execution-role **instance**: the object blocker 1 said did not exist.
///
/// A cycle declares that a `qa` role exists and may be attempted three times.
/// That is a type and a budget. This names *one* of those attempt slots, bound
/// to the work it serves and the repository point it concerns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleInstance {
    /// Sealed cycle digest the role is declared by.
    pub cycle: String,
    /// Role name as the cycle declares it, e.g. `implementation`, `qa`.
    pub role: String,
    /// Which attempt slot of that role. An ordinal, not an identity — the
    /// instance's identity is the whole tuple.
    pub ordinal: u32,
    /// Teko work artifact digest. Binding to declared work.
    pub work: String,
    /// Repository point the instance concerns. Binding to workflow context.
    pub repository_point: String,
}

impl RoleInstance {
    /// A deterministic, content-derived name for this instance.
    ///
    /// **Deliberately guessable.** A worker can compute this from public inputs,
    /// and that is fine: the id is not a secret and never carries authority.
    /// What a worker cannot produce is a signed [`Allocation`] naming it. Making
    /// the id unpredictable would be security theatre — an unguessable name that
    /// anybody may assert is worth exactly as much as a guessable one.
    pub fn name(&self) -> String {
        format!(
            "{}/{}#{}@{}",
            self.cycle, self.role, self.ordinal, self.repository_point
        )
    }
}

/// A statement that one role instance was allocated to be executed once.
///
/// The signed payload. It closes blocker 2 only when the signature verifies
/// against a trust root **outside the repository being governed** — a repository
/// must not decide who may allocate work in it. See
/// `docs/authority-chain.md` for the residual.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allocation {
    /// Namespace the detached signature must have been made under.
    pub namespace: String,
    /// The instance being allocated.
    pub instance: RoleInstance,
    /// Identity the verifier must accept from the external trust root.
    pub allocator: String,
    /// RFC 3339 UTC. Required: absence must never mean "forever".
    pub expires_at: String,
    /// Distinguishes two allocations of the same instance, so that spending one
    /// does not silently authorise the other. Not a secret.
    pub nonce: String,
}

impl Allocation {
    /// Whether this allocation is under the namespace a verifier must require.
    ///
    /// A well-formed allocation carrying another namespace is not an error to
    /// tolerate: it is a signature made for a different purpose being presented
    /// for this one.
    pub fn namespace_is_correct(&self) -> bool {
        self.namespace == ALLOCATION_NAMESPACE
    }

    /// The identity a spend record must be keyed by, so a replay of the same
    /// allocation is detectable by a verifier that can read prior spends.
    pub fn spend_key(&self) -> String {
        format!("{}!{}", self.instance.name(), self.nonce)
    }
}

/// What a verifier established about an allocation presented to it.
///
/// `Presented` exists so that "the worker handed me a document" is never
/// silently the same value as "an external allocator issued this".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AllocationState {
    /// A document was supplied. Nothing has been verified.
    Presented,
    /// Signature verified against an external trust root, unexpired, unspent.
    Verified { allocator: String },
    /// Verified, but this allocation has already been spent. Replay.
    AlreadySpent,
    /// Signature did not verify, or the namespace was wrong.
    Untrusted { reason: String },
    /// Verification could not run. Not a refusal, and not an acceptance.
    Indeterminate { reason: String },
}

impl AllocationState {
    /// Only a verified, unspent allocation can support composition allocation.
    ///
    /// `Indeterminate` is deliberately false here and must not be collapsed into
    /// `Untrusted` by callers: "I could not check" and "this failed" call for
    /// different responses.
    pub fn supports_allocation(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance() -> RoleInstance {
        RoleInstance {
            cycle: "sha256:cycle".into(),
            role: "qa".into(),
            ordinal: 1,
            work: "sha256:work".into(),
            repository_point: "rpoint_abc".into(),
        }
    }

    #[test]
    fn an_instance_names_its_work_and_its_point() {
        let instance = instance();
        assert!(instance.name().contains("qa"));
        assert!(instance.name().contains("rpoint_abc"));
        // Binding to declared work is carried, even though the name elides it:
        // the name is a label, the struct is the binding.
        assert_eq!(instance.work, "sha256:work");
    }

    #[test]
    fn instance_names_are_deterministic_and_that_is_intended() {
        assert_eq!(instance().name(), instance().name());
    }

    #[test]
    fn a_differently_namespaced_allocation_is_not_acceptable() {
        let mut allocation = Allocation {
            namespace: ALLOCATION_NAMESPACE.into(),
            instance: instance(),
            allocator: "yair".into(),
            expires_at: "2026-09-18T00:00:00Z".into(),
            nonce: "n1".into(),
        };
        assert!(allocation.namespace_is_correct());
        // A Kisko judgement signature must not be presentable as an allocation.
        allocation.namespace = "kisko-decision-v1".into();
        assert!(!allocation.namespace_is_correct());
    }

    #[test]
    fn the_nonce_separates_two_allocations_of_one_instance() {
        let first = Allocation {
            namespace: ALLOCATION_NAMESPACE.into(),
            instance: instance(),
            allocator: "yair".into(),
            expires_at: "2026-09-18T00:00:00Z".into(),
            nonce: "n1".into(),
        };
        let second = Allocation {
            nonce: "n2".into(),
            ..first.clone()
        };
        assert_ne!(first.spend_key(), second.spend_key());
    }

    #[test]
    fn only_a_verified_allocation_supports_allocation() {
        assert!(!AllocationState::Presented.supports_allocation());
        assert!(!AllocationState::AlreadySpent.supports_allocation());
        assert!(!AllocationState::Untrusted {
            reason: "bad signature".into()
        }
        .supports_allocation());
        assert!(!AllocationState::Indeterminate {
            reason: "ssh-keygen unavailable".into()
        }
        .supports_allocation());
        assert!(AllocationState::Verified {
            allocator: "yair".into()
        }
        .supports_allocation());
    }

    /// The confirmed attack, restated: a worker can construct the document.
    /// What it cannot construct is the state.
    #[test]
    fn a_worker_constructed_allocation_is_only_presented() {
        let forged = Allocation {
            namespace: ALLOCATION_NAMESPACE.into(),
            instance: instance(),
            allocator: "yair".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
            nonce: "whatever-the-worker-typed".into(),
        };
        assert!(forged.namespace_is_correct());
        // Well-formed, correctly namespaced, and worth nothing until a verifier
        // checks a detached signature against an external trust root.
        assert!(!AllocationState::Presented.supports_allocation());
    }
}
