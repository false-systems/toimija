//! The packet's seal: an envelope under the packet's `seal` key, carrying the
//! digest of the packet's canonical form, its provenance, its producer and the
//! consistency token it is anchored to.
//!
//! The envelope format is named `toimija-seal.v1` ([`ENVELOPE_SCHEMA`]), and
//! `docs/schemas/toimija-seal.v1.schema.json` describes it.
//!
//! This module names what a seal's digest covers and supplies Toimija's
//! producer identity; nothing here hashes. The canonical form and the digest
//! over it are `crate::canonical`. The digest covers the whole artifact, the
//! envelope included, without the envelope's own `digest`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::canonical::{canonical_json, digest_json, digest_value, is_sha256};

/// The name of the envelope format, carried in every seal as `envelope`.
pub const ENVELOPE_SCHEMA: &str = "toimija-seal.v1";

/// The envelope a sealed artifact carries under [`KEY`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Envelope {
    /// The envelope format: [`ENVELOPE_SCHEMA`].
    pub envelope: String,
    /// The schema of the artifact that carries the seal.
    pub schema: String,
    pub digest: String,
    #[serde(default)]
    pub provenance: Vec<Provenance>,
    pub producer: Producer,
    /// The consistency token the artifact is pinned to.
    pub anchored_at: String,
}

/// Who sealed the artifact. Self-declared: nothing here authenticates it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Producer {
    pub tool: String,
    pub version: String,
    pub run: Option<String>,
}

/// What a sealed artifact was built from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    Git {
        oid: String,
    },
    Token {
        token: String,
    },
    Artifact {
        digest: String,
    },
    /// Any edge kind this version does not name. Kept last and untagged, so
    /// an edge of a kind that is new to this reader does not make it refuse
    /// the whole artifact.
    #[serde(untagged)]
    Other(BTreeMap<String, Value>),
}

/// The producer Toimija stamps on what it seals.
pub const TOOL: &str = "toimija";

/// The key a sealed artifact carries its envelope under.
pub const KEY: &str = "seal";

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

/// Seal `artifact` as Toimija: attach an envelope and set its digest.
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

/// A seal is [`ENVELOPE_SCHEMA`] and nothing else.
fn supported(seal: &Envelope) -> serde_json::Result<()> {
    use serde::de::Error as _;
    if seal.envelope == ENVELOPE_SCHEMA {
        Ok(())
    } else {
        Err(serde_json::Error::custom(format!(
            "unsupported seal envelope `{}`; a seal is `{ENVELOPE_SCHEMA}`. Run `toimija refresh` \
             to replace the packet",
            seal.envelope
        )))
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
        assert_eq!(seal.producer.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(seal.envelope, ENVELOPE_SCHEMA);
        assert!(verify(&mut sample).unwrap().is_none());
        let value = serde_json::to_value(&sample).unwrap();
        assert!(verify_value(&value).unwrap().is_none());
    }

    /// The digest covers the seal itself, minus its own digest.
    #[test]
    fn the_digest_is_the_canonical_digest_of_the_artifact_without_the_seals_digest() {
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
