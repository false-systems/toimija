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
    let canonical = serde_json::to_string(artifact)?;
    *artifact.sauma_mut() = Some(envelope_from_canonical(
        schema,
        &canonical,
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
    let canonical = serde_json::to_string(artifact)?;
    *artifact.sauma_mut() = Some(envelope.clone());
    let actual = digest_json(&canonical);
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

pub fn envelope(
    schema: &str,
    canonical_json_without_sauma: &str,
    provenance: Vec<SaumaProvenance>,
    anchored_at: &str,
    run: Option<String>,
) -> SaumaEnvelope {
    envelope_from_canonical(
        schema,
        canonical_json_without_sauma,
        provenance,
        anchored_at,
        run,
    )
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
