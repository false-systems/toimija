//! Toimija's seam onto the canonical crate.
//!
//! The envelope, its canonical byte string and every digest over it live in
//! `sauma-canonical`, vendored under `vendor/` and pinned by
//! `vendor/sauma-canonical.contract`. This module is the adapter: it supplies
//! Toimija's producer identity and re-exports the rest, so call sites are
//! unchanged and nothing here hashes.
//!
//! Before this, Toimija carried its own copy of that algorithm — byte-identical
//! to teko's and to sauma's apart from comment wording, with nothing forcing
//! them to stay that way.

pub use sauma_canonical::{
    Carrier, ENVELOPE_SCHEMA, LEGACY_ENVELOPE_SCHEMA, SaumaEnvelope, SaumaProducer,
    SaumaProvenance, artifact_digest_value, canonical_json, digest_json, digest_value, is_sha256,
    is_supported_envelope, verify, verify_value,
};

/// The producer Toimija stamps on what it seals.
pub const TOOL: &str = "toimija";

fn producer(run: Option<String>) -> SaumaProducer {
    SaumaProducer {
        tool: TOOL.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        run,
    }
}

/// Seal an artifact as Toimija.
///
/// `sauma_canonical::attach` takes the producer rather than deriving it: read
/// from inside that crate, `env!("CARGO_PKG_VERSION")` would be the canonical
/// crate's version, not Toimija's, and every artifact would have claimed the
/// wrong origin. The identity is supplied here, where it is true.
pub fn attach<T: Carrier>(
    artifact: &mut T,
    schema: &str,
    provenance: Vec<SaumaProvenance>,
    anchored_at: &str,
    run: Option<String>,
) -> serde_json::Result<()> {
    sauma_canonical::attach(artifact, schema, provenance, anchored_at, producer(run))
}

/// A v1 envelope, for the schema-conformance check in `tests/docs.rs`.
///
/// Nothing in Toimija emits v1 any more. This stays because `docs.rs` asserts
/// that the shape the published `sauma-envelope.v1.schema.json` describes is
/// the shape this code produces — a check that needs something to produce it.
/// It is the one place the producer identity is built outside `attach`, for the
/// same reason: the version has to be Toimija's.
pub fn legacy_envelope_from_canonical(
    schema: &str,
    canonical_json_without_sauma: &str,
    provenance: Vec<SaumaProvenance>,
    anchored_at: &str,
    run: Option<String>,
) -> SaumaEnvelope {
    SaumaEnvelope {
        envelope: LEGACY_ENVELOPE_SCHEMA.to_string(),
        schema: schema.to_string(),
        digest: digest_json(canonical_json_without_sauma),
        provenance,
        producer: producer(run),
        anchored_at: anchored_at.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Sample {
        b: u8,
        a: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        sauma: Option<SaumaEnvelope>,
    }

    impl Carrier for Sample {
        fn sauma_mut(&mut self) -> &mut Option<SaumaEnvelope> {
            &mut self.sauma
        }
    }

    #[test]
    fn a_sealed_artifact_names_toimija_and_its_own_version() {
        let mut sample = Sample {
            b: 2,
            a: 1,
            sauma: None,
        };
        attach(&mut sample, "demo.v1", Vec::new(), "sha256:00", None).unwrap();
        let envelope = sample.sauma.clone().unwrap();
        assert_eq!(envelope.producer.tool, "toimija");
        // Not the canonical crate's version — the bug this signature prevents.
        assert_eq!(envelope.producer.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(envelope.envelope, ENVELOPE_SCHEMA);
        assert!(verify(&mut sample).unwrap().is_none());
    }

    #[test]
    fn tampering_after_the_seal_is_visible() {
        let mut sample = Sample {
            b: 2,
            a: 1,
            sauma: None,
        };
        attach(&mut sample, "demo.v1", Vec::new(), "sha256:00", None).unwrap();
        sample.a = 9;
        let (claimed, actual) = verify(&mut sample).unwrap().expect("digest must not match");
        assert_ne!(claimed, actual);
    }
}
