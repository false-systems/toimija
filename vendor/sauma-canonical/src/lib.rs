//! The canonical crate for the False Systems workflow stack.
//!
//! One envelope, one canonical byte string, one digest — in one place. Every
//! tool that seals or checks an artifact links this crate rather than carrying
//! its own copy.
//!
//! # Why this exists
//!
//! The algorithm below was, until this crate, implemented four times:
//! `sauma/src/lib.rs`, `toimija/src/sauma.rs`, `teko/src/sauma.rs` and
//! `kelpo/kelpo.py`. The three Rust copies were byte-identical apart from
//! comment wording, which is the dangerous kind of duplication: nothing forced
//! them to stay that way, and a digest that disagrees between two tools breaks
//! the chain of custody silently — the artifact simply stops verifying, with no
//! indication which side is wrong.
//!
//! Python is the copy that measurably disagreed. `tests/conformance.rs` pins
//! the cases where `json.dumps` and `serde_json` differ, so Kelpo's agreement is
//! proven rather than assumed.
//!
//! # The rule
//!
//! Nothing outside this crate hashes an artifact. `deny.toml` enforces it by
//! refusing any other crate a direct dependency on `sha2`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const LEGACY_ENVELOPE_SCHEMA: &str = "sauma-envelope.v1";
pub const ENVELOPE_SCHEMA: &str = "sauma-envelope.v2";

/// Whether a schema string names an envelope version this stack still reads.
///
/// v1 is accepted for verification only; nothing emits it.
pub fn is_supported_envelope(schema: &str) -> bool {
    matches!(schema, LEGACY_ENVELOPE_SCHEMA | ENVELOPE_SCHEMA)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SaumaEnvelope {
    pub envelope: String,
    pub schema: String,
    pub digest: String,
    #[serde(default)]
    pub provenance: Vec<SaumaProvenance>,
    pub producer: SaumaProducer,
    pub anchored_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SaumaProducer {
    pub tool: String,
    pub version: String,
    pub run: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SaumaProvenance {
    Git {
        oid: String,
    },
    Token {
        token: String,
    },
    Artifact {
        digest: String,
    },
    Spec {
        id: String,
    },
    Work {
        id: String,
    },
    Session {
        id: String,
    },
    WorkerRun {
        id: String,
    },
    RepositoryPoint {
        id: String,
    },
    External {
        r#ref: String,
    },
    /// Any edge kind this version does not name. Kept last and untagged so a
    /// producer can add an edge kind without the verifier refusing the whole
    /// artifact — an unknown edge is still a walkable edge.
    #[serde(untagged)]
    Other(BTreeMap<String, Value>),
}

impl SaumaProvenance {
    pub fn kind(&self) -> &str {
        match self {
            Self::Git { .. } => "git",
            Self::Token { .. } => "token",
            Self::Artifact { .. } => "artifact",
            Self::Spec { .. } => "spec",
            Self::Work { .. } => "work",
            Self::Session { .. } => "session",
            Self::WorkerRun { .. } => "worker_run",
            Self::RepositoryPoint { .. } => "repository_point",
            Self::External { .. } => "external",
            Self::Other(fields) => fields
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
        }
    }

    pub fn link_value(&self) -> Option<String> {
        match self {
            Self::Git { oid } => Some(oid.clone()),
            Self::Token { token } => Some(token.clone()),
            Self::Artifact { digest } => Some(digest.clone()),
            Self::Spec { id }
            | Self::Work { id }
            | Self::Session { id }
            | Self::WorkerRun { id }
            | Self::RepositoryPoint { id } => Some(id.clone()),
            Self::External { r#ref } => Some(r#ref.clone()),
            Self::Other(fields) => ["digest", "token", "oid", "id", "ref"]
                .iter()
                .find_map(|key| fields.get(*key).and_then(Value::as_str))
                .map(str::to_string),
        }
    }
}

pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(&mut out, value);
    out
}

fn write_canonical(out: &mut String, value: &Value) {
    match value {
        Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(out, &map[key]);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(out, item);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

pub fn is_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

pub fn digest_json(json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

pub fn digest_value(value: &Value) -> String {
    digest_json(&canonical_json(value))
}

pub fn artifact_digest_value(value: &Value) -> String {
    let mut covered = value.clone();
    if let Value::Object(map) = &mut covered {
        if map
            .get("sauma")
            .and_then(|envelope| envelope.get("envelope"))
            .and_then(Value::as_str)
            == Some(ENVELOPE_SCHEMA)
        {
            if let Some(envelope) = map.get_mut("sauma").and_then(Value::as_object_mut) {
                envelope.remove("digest");
            }
        } else {
            map.remove("sauma");
        }
    }
    digest_value(&covered)
}
fn unsupported(envelope: &SaumaEnvelope) -> serde_json::Result<()> {
    use serde::de::Error as _;
    if is_supported_envelope(&envelope.envelope) {
        Ok(())
    } else {
        Err(serde_json::Error::custom(format!(
            "unsupported Sauma envelope `{}`",
            envelope.envelope
        )))
    }
}

/// An artifact that carries an envelope. Implemented by each tool's own types.
pub trait Carrier: Serialize {
    fn sauma_mut(&mut self) -> &mut Option<SaumaEnvelope>;
}

/// Seal `artifact`: attach an envelope and set its digest.
///
/// The producer is supplied by the caller rather than derived here. Moving this
/// function into a shared crate turned `env!("CARGO_PKG_VERSION")` from "the
/// tool's version" into "this crate's version" — every artifact would have
/// claimed to come from sauma-canonical 0.1.0. Identity belongs to whoever is
/// making the claim; only the algorithm belongs here.
pub fn attach<T: Carrier>(
    artifact: &mut T,
    schema: &str,
    provenance: Vec<SaumaProvenance>,
    anchored_at: &str,
    producer: SaumaProducer,
) -> serde_json::Result<()> {
    *artifact.sauma_mut() = Some(SaumaEnvelope {
        envelope: ENVELOPE_SCHEMA.to_string(),
        schema: schema.to_string(),
        digest: String::new(),
        provenance,
        producer,
        anchored_at: anchored_at.to_string(),
    });
    let value = serde_json::to_value(&*artifact)?;
    artifact.sauma_mut().as_mut().unwrap().digest = artifact_digest_value(&value);
    Ok(())
}

/// Recompute a carrier's digest. `Some((claimed, actual))` means tampering.
pub fn verify<T: Carrier>(artifact: &mut T) -> serde_json::Result<Option<(String, String)>> {
    let Some(envelope) = artifact.sauma_mut().clone() else {
        return Ok(None);
    };
    unsupported(&envelope)?;
    let value = serde_json::to_value(&*artifact)?;
    let actual = artifact_digest_value(&value);
    Ok((actual != envelope.digest).then_some((envelope.digest, actual)))
}

/// Verify serialized bytes before typed deserialization can materialize
/// defaults for additive fields that were absent when an older producer signed
/// it. The digest covers the bytes' JSON value, not a newer reader's model.
pub fn verify_value(value: Value) -> serde_json::Result<Option<(String, String)>> {
    use serde::de::Error as _;
    let Some(envelope) = value.get("sauma").cloned() else {
        return Ok(None);
    };
    let envelope: SaumaEnvelope = serde_json::from_value(envelope)?;
    unsupported(&envelope)?;
    if value
        .get("schema")
        .or_else(|| value.get("record"))
        .and_then(Value::as_str)
        .is_some_and(|schema| schema != envelope.schema)
    {
        return Err(serde_json::Error::custom(
            "Sauma envelope schema differs from artifact schema",
        ));
    }
    let actual = artifact_digest_value(&value);
    Ok((actual != envelope.digest).then_some((envelope.digest, actual)))
}

/// Why a value has no canonical form this stack can agree on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotCanonicalizable {
    /// JSON-pointer-ish path to the offending value, e.g. `/body/score`.
    pub path: String,
    pub reason: &'static str,
}

impl std::fmt::Display for NotCanonicalizable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at {}",
            self.reason,
            if self.path.is_empty() {
                "/"
            } else {
                &self.path
            }
        )
    }
}

/// Whether every value here canonicalizes to the same bytes in every
/// implementation of this stack.
///
/// Only numbers can fail, and only in one way: a JSON number that is not an
/// exact `i64`/`u64`. Two measured cases, both silent before this check:
///
/// * `1.0e-7` — `serde_json` writes `1e-7`, Python's `json.dumps` writes
///   `1e-07`. Different bytes, different sha256.
/// * `123456789012345678901234567890` — beyond `f64`'s exact range, so
///   `serde_json` already rounded it to `1.2345678901234568e+29` at parse time
///   while Python kept the digits.
///
/// Floats have no interoperable shortest-representation rule across languages,
/// so rather than pick one and hope, an artifact that contains one is refused.
/// Everything the stack seals today — strings, digests, booleans, timestamps,
/// counts, durations — is unaffected.
pub fn check_canonicalizable(value: &Value) -> Result<(), NotCanonicalizable> {
    fn walk(value: &Value, path: &mut String) -> Result<(), NotCanonicalizable> {
        match value {
            Value::Number(number) => {
                if number.as_i64().is_none() && number.as_u64().is_none() {
                    return Err(NotCanonicalizable {
                        path: path.clone(),
                        reason: "a non-integer JSON number has no canonical form every \
                                 implementation agrees on",
                    });
                }
                Ok(())
            }
            Value::Array(items) => {
                let len = path.len();
                for (index, item) in items.iter().enumerate() {
                    path.push('/');
                    path.push_str(&index.to_string());
                    walk(item, path)?;
                    path.truncate(len);
                }
                Ok(())
            }
            Value::Object(map) => {
                let len = path.len();
                for (key, item) in map {
                    path.push('/');
                    path.push_str(key);
                    walk(item, path)?;
                    path.truncate(len);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    let mut path = String::new();
    walk(value, &mut path)
}
