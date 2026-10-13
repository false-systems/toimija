//! Who produced an artifact, and whether anyone checked.
//!
//! A `sauma-envelope.v1` is tamper-evident and **not** authenticated: it carries
//! a digest and a self-declared producer name, so anyone who can write the file
//! can write the envelope. Every artifact in this stack has that property.
//!
//! # What a signature buys, and what it does not
//!
//! A signature separates a producer from a claimant **only while the claimant
//! cannot produce it**. Where an agent and the signing key live on one host
//! under one user, the agent can sign its own work and the signature separates
//! nothing — it adds identity without independence.
//!
//! So this module does not make signing mandatory. It makes the distinction
//! *representable and enforceable*, so a deployment with a real isolation
//! boundary can require it, and so an unsigned artifact can never be mistaken
//! for a signed one. Where there is no isolation, the honest configuration is
//! [`TrustRequirement::TamperEvident`] — and the verdict says so rather than
//! implying more.

use serde::{Deserialize, Serialize};

/// A detached signature over an artifact's canonical form.
///
/// Current assurance signatures cover the artifact except their circular
/// signature and digest fields. Historical v1 signatures covered only the
/// body, so the verified namespace travels with the result.
///
/// Reuses the OpenSSH machinery the estate already has for engineering
/// judgement rather than introducing a second scheme. The namespace is part of
/// the statement: a signature made over one kind of claim must not be
/// presentable as a signature over another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    /// The signing namespace, e.g. `faija-assurance-v1`.
    pub namespace: String,
    /// The identity the verifier must accept.
    pub signer: String,
    /// Armored detached signature.
    pub signature: String,
}

/// What a verifier established about an artifact's producer.
///
/// `Unauthenticated` is a real answer, not a failure: most artifacts in this
/// stack are legitimately unsigned. What matters is that it is never confused
/// with `Verified`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "authentication", rename_all = "snake_case")]
pub enum Authentication {
    /// No signature was offered. Tamper-evident only.
    Unauthenticated,
    /// A signature verified against a trust root outside the governed
    /// repository — a repository must not decide who may attest work in it.
    Verified {
        signer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    /// A signature was offered and did not verify.
    Untrusted { reason: String },
    /// A signature was offered and could not be checked here. Distinct from
    /// `Untrusted`: "I could not look" is not "this failed".
    Indeterminate { reason: String },
}

impl Authentication {
    /// Whether a producer identity was established.
    pub fn establishes_producer(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }

    /// The identity, when one was established.
    pub fn signer(&self) -> Option<&str> {
        match self {
            Self::Verified { signer, .. } => Some(signer.as_str()),
            _ => None,
        }
    }
}

/// How much a consumer requires of an artifact's producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "trust", rename_all = "snake_case")]
pub enum TrustRequirement {
    /// Integrity only. The honest default where the producer and the verifier
    /// share a host and a user, because a signature there separates nothing.
    #[default]
    TamperEvident,
    /// A verified signature from one of these identities.
    ///
    /// Meaningful only where the signing capability is genuinely outside the
    /// claimant's reach. Configuring this without that isolation records a
    /// stronger-looking claim that is not stronger.
    Authenticated { allowed: Vec<String> },
}

impl TrustRequirement {
    /// Whether this authentication satisfies the requirement.
    ///
    /// `TamperEvident` accepts anything including a failed signature — it asked
    /// for no producer identity, and a broken signature is not worse than the
    /// absence it tolerates. It does *not* mean the failure is hidden: the
    /// [`Authentication`] travels with the artifact either way.
    pub fn satisfied_by(&self, authentication: &Authentication) -> bool {
        match self {
            Self::TamperEvident => true,
            Self::Authenticated { allowed } => authentication
                .signer()
                .is_some_and(|signer| allowed.iter().any(|entry| entry == signer)),
        }
    }

    /// Why the requirement was not met, for a refusal that says something.
    pub fn refusal(&self, authentication: &Authentication) -> Option<String> {
        if self.satisfied_by(authentication) {
            return None;
        }
        Some(match authentication {
            Authentication::Unauthenticated => {
                "this consumer requires a signed artifact and none was offered".into()
            }
            Authentication::Untrusted { reason } => {
                format!("the signature did not verify: {reason}")
            }
            Authentication::Indeterminate { reason } => {
                format!("the signature could not be checked: {reason}")
            }
            Authentication::Verified { signer, .. } => {
                format!("`{signer}` is not an allowed signer for this consumer")
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verified(signer: &str) -> Authentication {
        Authentication::Verified {
            signer: signer.into(),
            namespace: None,
        }
    }

    #[test]
    fn unsigned_never_establishes_a_producer() {
        assert!(!Authentication::Unauthenticated.establishes_producer());
        assert!(!Authentication::Untrusted {
            reason: "bad key".into()
        }
        .establishes_producer());
        assert!(!Authentication::Indeterminate {
            reason: "ssh-keygen missing".into()
        }
        .establishes_producer());
        assert!(verified("yair").establishes_producer());
    }

    /// The default is honest about a single-user host rather than performing
    /// rigor a signature there would not provide.
    #[test]
    fn the_default_requires_integrity_not_identity() {
        let requirement = TrustRequirement::default();
        assert_eq!(requirement, TrustRequirement::TamperEvident);
        assert!(requirement.satisfied_by(&Authentication::Unauthenticated));
        assert!(requirement
            .refusal(&Authentication::Unauthenticated)
            .is_none());
    }

    #[test]
    fn an_unsigned_artifact_cannot_satisfy_an_authenticated_requirement() {
        let requirement = TrustRequirement::Authenticated {
            allowed: vec!["yair".into()],
        };
        assert!(!requirement.satisfied_by(&Authentication::Unauthenticated));
        assert!(requirement
            .refusal(&Authentication::Unauthenticated)
            .expect("a reason")
            .contains("none was offered"));
    }

    #[test]
    fn a_signature_from_an_unlisted_identity_does_not_satisfy() {
        let requirement = TrustRequirement::Authenticated {
            allowed: vec!["yair".into()],
        };
        assert!(!requirement.satisfied_by(&verified("someone-else")));
        assert!(requirement.satisfied_by(&verified("yair")));
        assert!(requirement
            .refusal(&verified("someone-else"))
            .expect("a reason")
            .contains("not an allowed signer"));
    }

    /// "Could not check" and "failed" call for different responses and keep
    /// different names, even where the requirement tolerates both.
    #[test]
    fn indeterminate_is_not_untrusted() {
        let indeterminate = Authentication::Indeterminate {
            reason: "ssh-keygen unavailable".into(),
        };
        let untrusted = Authentication::Untrusted {
            reason: "signature did not verify".into(),
        };
        assert_ne!(indeterminate, untrusted);
        let requirement = TrustRequirement::Authenticated {
            allowed: vec!["yair".into()],
        };
        assert!(requirement
            .refusal(&indeterminate)
            .expect("a reason")
            .contains("could not be checked"));
        assert!(requirement
            .refusal(&untrusted)
            .expect("a reason")
            .contains("did not verify"));
    }
}
