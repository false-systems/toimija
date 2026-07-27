use serde::{Deserialize, Serialize};

use crate::consistency;

pub const ENVELOPE_SCHEMA: &str = "sauma-envelope.v1";
pub const TOOL: &str = "toimija";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SaumaEnvelope {
    pub envelope: String,
    pub schema: String,
    pub digest: String,
    pub provenance: Vec<SaumaProvenance>,
    pub producer: SaumaProducer,
    pub anchored_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SaumaProvenance {
    Git { oid: String },
    Token { token: String },
    Artifact { digest: String },
    Spec { id: String },
    Work { id: String },
    Session { id: String },
    WorkerRun { id: String },
    RepositoryPoint { id: String },
    External { r#ref: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SaumaProducer {
    pub tool: String,
    pub version: String,
    pub run: Option<String>,
}

pub fn digest_json(json: &str) -> String {
    consistency::hash_bytes(json.as_bytes())
}

/// The sauma canonical serialization: compact JSON with object keys sorted
/// lexicographically at every level. This is the one canonical form every
/// producer digests over (proposal 0003 §2/§9 D-P2), so a digest is a portable
/// content-address any walker can verify regardless of a crate's `serde_json`
/// key-ordering (toimija enables `preserve_order`; kisko does not). No
/// number/unicode normalization — sorted keys and compact spacing only.
pub fn canonical_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(&mut out, value);
    out
}

fn write_canonical(out: &mut String, value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                // Serialize the key as a JSON string for correct escaping.
                out.push_str(&serde_json::Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(out, &map[key]);
            }
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(out, item);
            }
            out.push(']');
        }
        // Scalars: serde_json's own compact serialization is already canonical.
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// Digest an artifact value over the sauma canonical form. The caller is
/// responsible for having removed the top-level `sauma` field first.
pub fn digest_value(value: &serde_json::Value) -> String {
    digest_json(&canonical_json(value))
}

pub trait Carrier: Serialize {
    fn sauma_mut(&mut self) -> &mut Option<SaumaEnvelope>;
}

pub fn attach<T: Carrier>(
    artifact: &mut T,
    schema: &str,
    provenance: Vec<SaumaProvenance>,
    anchored_at: &str,
    run: Option<String>,
) -> serde_json::Result<()> {
    *artifact.sauma_mut() = None;
    let value = serde_json::to_value(&*artifact)?;
    *artifact.sauma_mut() = Some(envelope_from_canonical(
        schema,
        &canonical_json(&value),
        provenance,
        anchored_at,
        run,
    ));
    Ok(())
}

pub fn verify<T: Carrier>(artifact: &mut T) -> serde_json::Result<Option<(String, String)>> {
    let Some(envelope) = artifact.sauma_mut().clone() else {
        return Ok(None);
    };
    *artifact.sauma_mut() = None;
    let value = serde_json::to_value(&*artifact)?;
    *artifact.sauma_mut() = Some(envelope.clone());
    let actual = digest_value(&value);
    Ok((actual != envelope.digest).then_some((envelope.digest, actual)))
}

/// Verify a serialized artifact before typed deserialization can materialize
/// defaults for additive fields that were absent when an older producer signed
/// it. The digest covers the bytes' JSON value, not a newer reader's model.
pub fn verify_value(mut value: serde_json::Value) -> serde_json::Result<Option<(String, String)>> {
    let Some(envelope) = value
        .as_object_mut()
        .and_then(|object| object.remove("sauma"))
    else {
        return Ok(None);
    };
    let envelope: SaumaEnvelope = serde_json::from_value(envelope)?;
    let actual = digest_value(&value);
    Ok((actual != envelope.digest).then_some((envelope.digest, actual)))
}

pub fn envelope_from_canonical(
    schema: &str,
    canonical_json_without_sauma: &str,
    provenance: Vec<SaumaProvenance>,
    anchored_at: &str,
    run: Option<String>,
) -> SaumaEnvelope {
    SaumaEnvelope {
        envelope: ENVELOPE_SCHEMA.to_string(),
        schema: schema.to_string(),
        digest: digest_json(canonical_json_without_sauma),
        provenance,
        producer: SaumaProducer {
            tool: TOOL.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            run,
        },
        anchored_at: anchored_at.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_prefixed_lowercase_sha256() {
        assert_eq!(
            digest_json("{}"),
            "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
    }

    #[test]
    fn raw_verification_does_not_materialize_new_reader_defaults() {
        let mut value = serde_json::json!({"schema": "old", "items": []});
        let envelope =
            envelope_from_canonical("old", &canonical_json(&value), Vec::new(), "token", None);
        value["sauma"] = serde_json::to_value(envelope).unwrap();

        assert_eq!(verify_value(value).unwrap(), None);
    }
}
