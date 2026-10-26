//! The packet's seal: an envelope under the packet's `seal` key, carrying the
//! digest of the packet's canonical form, its provenance, its producer and the
//! consistency token it is anchored to.
//!
//! The envelope, its canonical byte string and every digest over it live in the
//! vendored canonical crate (`vendor/sauma-canonical`, pinned by
//! `vendor/sauma-canonical.contract`). Sauma wrote it and was archived on
//! 2026-10-13; Toimija keeps the code as its own. The envelope format keeps the
//! name it was registered under, `sauma-envelope.v2`.
//!
//! This module names what a seal's digest covers and supplies Toimija's
//! producer identity; nothing here hashes. The crate's own `attach` and
//! `verify_value` read the envelope under the key `sauma`, where
//! `toimija-context.v2` kept it, so for `seal` the two are done here, with the
//! crate's [`digest_value`] and the crate's v2 rule: the digest covers the
//! whole artifact, the envelope included, without the envelope's own `digest`.

use serde::Serialize;
use serde_json::Value;

pub use sauma_canonical::{
    ENVELOPE_SCHEMA, LEGACY_ENVELOPE_SCHEMA, SaumaEnvelope as Envelope, SaumaProducer as Producer,
    SaumaProvenance as Provenance, canonical_json, digest_json, digest_value, is_sha256,
    is_supported_envelope,
};

/// The producer Toimija stamps on what it seals.
pub const TOOL: &str = "toimija";

/// The key a sealed artifact carries its envelope under.
pub const KEY: &str = "seal";

/// The key other tools' artifacts (kisko's model calls, teko's work, kelpo's
/// verdicts) carry the same envelope under, as `toimija-context.v2` did. A
/// walker over the local chain reads both.
pub const SAUMA_KEY: &str = "sauma";

/// The digest an envelope under [`SAUMA_KEY`] carries, by the crate's rules
/// for that key: a `sauma-envelope.v2` digest covers the envelope, a v1
/// digest leaves it out.
pub use sauma_canonical::artifact_digest_value as digest_under_sauma_key;

/// An artifact that carries a seal under [`KEY`].
pub trait Sealed: Serialize {
    fn seal_mut(&mut self) -> &mut Option<Envelope>;
}

fn producer(run: Option<String>) -> Producer {
    Producer {
        tool: TOOL.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        run,
    }
}

/// The digest a seal over `value` carries: the canonical digest of the whole
/// value, the seal included, without the seal's own `digest`.
pub fn digest_of(value: &Value) -> String {
    let mut covered = value.clone();
    if let Some(seal) = covered.get_mut(KEY).and_then(Value::as_object_mut) {
        seal.remove("digest");
    }
    digest_value(&covered)
}

/// Seal `artifact` as Toimija: attach a `sauma-envelope.v2` and set its digest.
pub fn attach<T: Sealed>(
    artifact: &mut T,
    schema: &str,
    provenance: Vec<Provenance>,
    anchored_at: &str,
    run: Option<String>,
) -> serde_json::Result<()> {
    *artifact.seal_mut() = Some(Envelope {
        envelope: ENVELOPE_SCHEMA.to_string(),
        schema: schema.to_string(),
        digest: String::new(),
        provenance,
        producer: producer(run),
        anchored_at: anchored_at.to_string(),
    });
    let digest = digest_of(&serde_json::to_value(&*artifact)?);
    artifact.seal_mut().as_mut().expect("attached above").digest = digest;
    Ok(())
}

/// Recompute a sealed artifact's digest. `Some((claimed, actual))` means the
/// artifact changed after it was sealed; `None` without a seal.
pub fn verify<T: Sealed>(artifact: &mut T) -> serde_json::Result<Option<(String, String)>> {
    let Some(seal) = artifact.seal_mut().clone() else {
        return Ok(None);
    };
    supported(&seal)?;
    let actual = digest_of(&serde_json::to_value(&*artifact)?);
    Ok((actual != seal.digest).then_some((seal.digest, actual)))
}

/// Verify serialized bytes before typed deserialization can materialize
/// defaults for additive fields that were absent when an older producer sealed
/// them: the digest covers the bytes' JSON value, not a newer reader's model.
/// `Some((claimed, actual))` means the artifact changed after it was sealed;
/// `None` without a seal.
pub fn verify_value(value: &Value) -> serde_json::Result<Option<(String, String)>> {
    use serde::de::Error as _;
    let Some(seal) = value.get(KEY).cloned() else {
        return Ok(None);
    };
    let seal: Envelope = serde_json::from_value(seal)?;
    supported(&seal)?;
    if value
        .get("schema")
        .and_then(Value::as_str)
        .is_some_and(|schema| schema != seal.schema)
    {
        return Err(serde_json::Error::custom(
            "seal schema differs from packet schema",
        ));
    }
    let actual = digest_of(value);
    Ok((actual != seal.digest).then_some((seal.digest, actual)))
}

/// A seal is `sauma-envelope.v2`. The legacy v1 form, whose digest left the
/// envelope out, was only ever written under the `sauma` key.
fn supported(seal: &Envelope) -> serde_json::Result<()> {
    use serde::de::Error as _;
    if seal.envelope == ENVELOPE_SCHEMA {
        Ok(())
    } else {
        Err(serde_json::Error::custom(format!(
            "unsupported seal envelope `{}`; a seal is `{ENVELOPE_SCHEMA}`",
            seal.envelope
        )))
    }
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
    canonical_json_without_seal: &str,
    provenance: Vec<Provenance>,
    anchored_at: &str,
    run: Option<String>,
) -> Envelope {
    Envelope {
        envelope: LEGACY_ENVELOPE_SCHEMA.to_string(),
        schema: schema.to_string(),
        digest: digest_json(canonical_json_without_seal),
        provenance,
        producer: producer(run),
        anchored_at: anchored_at.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Sample {
        b: u8,
        a: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        seal: Option<Envelope>,
    }

    impl Sealed for Sample {
        fn seal_mut(&mut self) -> &mut Option<Envelope> {
            &mut self.seal
        }
    }

    fn sealed() -> Sample {
        let mut sample = Sample {
            b: 2,
            a: 1,
            seal: None,
        };
        attach(&mut sample, "demo.v1", Vec::new(), "sha256:00", None).unwrap();
        sample
    }

    #[test]
    fn a_sealed_artifact_names_toimija_and_its_own_version() {
        let mut sample = sealed();
        let seal = sample.seal.clone().unwrap();
        assert_eq!(seal.producer.tool, "toimija");
        // Not the canonical crate's version.
        assert_eq!(seal.producer.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(seal.envelope, ENVELOPE_SCHEMA);
        assert!(verify(&mut sample).unwrap().is_none());
        let value = serde_json::to_value(&sample).unwrap();
        assert!(value.get("sauma").is_none(), "{value}");
        assert!(verify_value(&value).unwrap().is_none());
    }

    /// The digest covers the seal itself, minus its own digest: the crate's
    /// v2 rule, applied under `seal` rather than `sauma`.
    #[test]
    fn the_digest_is_the_canonical_v2_digest_under_the_seal_key() {
        let sample = sealed();
        let value = serde_json::to_value(&sample).unwrap();
        let mut without_digest = value.clone();
        without_digest[KEY]
            .as_object_mut()
            .unwrap()
            .remove("digest");
        assert_eq!(
            sample.seal.as_ref().unwrap().digest,
            digest_value(&without_digest)
        );
        // Moving the producer, the provenance or the anchor moves the digest.
        let mut moved = value.clone();
        moved[KEY]["anchored_at"] = serde_json::json!("sha256:11");
        assert_ne!(digest_of(&moved), digest_of(&value));
    }

    #[test]
    fn tampering_after_the_seal_is_visible() {
        let mut sample = sealed();
        sample.a = 9;
        let (claimed, actual) = verify(&mut sample).unwrap().expect("digest must not match");
        assert_ne!(claimed, actual);
        let mut value = serde_json::to_value(sealed()).unwrap();
        value[KEY]["producer"]["tool"] = serde_json::json!("someone else");
        assert!(verify_value(&value).unwrap().is_some());
    }
}
