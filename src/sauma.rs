use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SaumaProducer {
    pub tool: String,
    pub version: String,
    pub run: Option<String>,
}

pub fn digest_json(json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

pub fn envelope(
    schema: &str,
    artifact_json_without_sauma: &str,
    provenance: Vec<SaumaProvenance>,
    anchored_at: &str,
    run: Option<String>,
) -> SaumaEnvelope {
    SaumaEnvelope {
        envelope: ENVELOPE_SCHEMA.to_string(),
        schema: schema.to_string(),
        digest: digest_json(artifact_json_without_sauma),
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
}
