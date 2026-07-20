//! Documentation guarantees. The Git-state thesis is part of the product, so
//! the README must state it verbatim.

const README: &str = include_str!("../README.md");
const HANDOFF_SCHEMA: &str = include_str!("../docs/schemas/handoff-record.v1.schema.json");
const HISTORY_SCHEMA: &str = include_str!("../docs/schemas/history-record.v1.schema.json");
const SAUMA_SCHEMA: &str = include_str!("../docs/schemas/sauma-envelope.v1.schema.json");

#[test]
fn readme_states_the_git_state_thesis() {
    assert!(
        README.contains(
            "We do not ask the model to understand the repo first. \
             We ask Git what state the repo is in."
        ),
        "README.md must contain the Git-state thesis sentence"
    );
}

#[test]
fn readme_states_git_is_the_substrate() {
    // The dedicated "Git as the starting point" section was folded into the
    // identity block (it triple-stated the thesis); the guarantee is the
    // substance, verbatim, not the heading.
    assert!(
        README.contains("Git is not a save button here. Git is the substrate."),
        "README.md must keep the Git-substrate statement"
    );
    assert!(
        README.contains(
            "Git gives us the graph of what happened. \
             Toimija turns that graph into operational context for agents."
        ),
        "README.md must keep the graph-to-context sentence"
    );
}

#[test]
fn handoff_schema_matches_emitted_record_fields() {
    use std::collections::BTreeSet;

    let schema: serde_json::Value = serde_json::from_str(HANDOFF_SCHEMA).unwrap();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(
        schema["$id"],
        "https://false.systems/schemas/handoff-record.v1.schema.json"
    );
    // The record schema permits unknown properties: the reader is tolerant
    // (`HandoffRecord` has no `deny_unknown_fields`) and the schema's own rule is
    // that new optional fields are additive within v1, so an older validator must
    // accept a newer-but-compatible record. Enforced end to end by
    // `handoff_schema_permits_additive_fields_like_the_reader`.
    assert_eq!(schema["additionalProperties"], true);

    let record = toimija::handoff::HandoffRecord {
        version: 1,
        session_id: "100-1".to_string(),
        written_at: 1,
        agent: Vec::new(),
        agent_total: 0,
        intent: None,
        branch: None,
        entry_head: None,
        head: None,
        merge_base: None,
        staged: Vec::new(),
        staged_total: 0,
        unstaged: Vec::new(),
        unstaged_total: 0,
        untracked: Vec::new(),
        untracked_total: 0,
        conflicts: Vec::new(),
        conflicts_total: 0,
        branch_scope: Vec::new(),
        branch_scope_total: 0,
        suggested_validation: Vec::new(),
        suggested_validation_total: 0,
        coupling: Vec::new(),
        coupling_total: 0,
        risk: Vec::new(),
        risk_total: 0,
        consistency_token: "token".to_string(),
        next_step: None,
        sauma: Some(toimija::sauma::envelope_from_canonical(
            "handoff-record.v1",
            "{}",
            Vec::new(),
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            Some("100-1".to_string()),
        )),
    };
    let emitted = serde_json::to_value(record).unwrap();
    let emitted_keys: BTreeSet<_> = emitted.as_object().unwrap().keys().cloned().collect();
    let property_keys: BTreeSet<_> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let required: BTreeSet<_> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect();

    let mut required_with_additive = required.clone();
    required_with_additive.insert("sauma".to_string());

    assert_eq!(emitted_keys, property_keys);
    assert_eq!(emitted_keys, required_with_additive);
    assert!(!required.contains("sauma"));
}

/// The handoff schema and the handoff reader must agree on forward-compatibility:
/// the schema permits unknown properties (`additionalProperties: true`) and the
/// reader parses a record carrying one. Without this pairing the two drift — a
/// strict schema rejecting exactly the additive v1 fields the reader and the
/// schema description promise to accept.
#[test]
fn handoff_schema_permits_additive_fields_like_the_reader() {
    let schema: serde_json::Value = serde_json::from_str(HANDOFF_SCHEMA).unwrap();
    assert_eq!(
        schema["additionalProperties"], true,
        "handoff schema must permit additive unknown fields"
    );

    // A complete v1 record plus one field the schema does not name. The tolerant
    // reader must still parse it, matching the schema's now-open stance.
    let with_unknown = r#"{"version":1,"session_id":"100-1","written_at":1,"agent":[],"agent_total":0,"intent":null,"branch":null,"entry_head":null,"head":null,"merge_base":null,"staged":[],"staged_total":0,"unstaged":[],"unstaged_total":0,"untracked":[],"untracked_total":0,"conflicts":[],"conflicts_total":0,"branch_scope":[],"branch_scope_total":0,"suggested_validation":[],"suggested_validation_total":0,"coupling":[],"coupling_total":0,"risk":[],"risk_total":0,"consistency_token":"t","next_step":null,"future_optional":"ok"}"#;
    let parsed: Result<toimija::handoff::HandoffRecord, _> = serde_json::from_str(with_unknown);
    assert!(
        parsed.is_ok(),
        "tolerant reader must accept a record with an unknown optional field"
    );
}

#[test]
fn history_schema_matches_emitted_record_fields() {
    use std::collections::BTreeSet;

    let schema: serde_json::Value = serde_json::from_str(HISTORY_SCHEMA).unwrap();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["$id"], "docs/schemas/history-record.v1.schema.json");
    assert_eq!(schema["additionalProperties"], false);

    let record = toimija::footprint::HistoryRecord {
        version: 1,
        id: "100-1".to_string(),
        agent: vec!["codex".to_string()],
        intent: None,
        scope: Vec::new(),
        branch: Some("main".to_string()),
        started_at: 1,
        last_seen_at: Some(2),
        end: toimija::footprint::HistoryEnd::Closed,
        exit_status: Some(0),
        handoffs: Vec::new(),
        handoffs_total: 0,
        recorded_at: 3,
    };
    let emitted = serde_json::to_value(record).unwrap();
    let emitted_keys: BTreeSet<_> = emitted.as_object().unwrap().keys().cloned().collect();
    let property_keys: BTreeSet<_> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let required: BTreeSet<_> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect();

    assert_eq!(emitted_keys, property_keys);
    assert_eq!(emitted_keys, required);
}

#[test]
fn sauma_schema_matches_emitted_envelope_fields() {
    use std::collections::BTreeSet;

    let schema: serde_json::Value = serde_json::from_str(SAUMA_SCHEMA).unwrap();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(
        schema["$id"],
        "https://false.systems/schemas/sauma-envelope.v1.schema.json"
    );
    assert_eq!(schema["additionalProperties"], false);

    let envelope = toimija::sauma::envelope_from_canonical(
        "handoff-record.v1",
        "{}",
        vec![toimija::sauma::SaumaProvenance::Token {
            token: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
        }],
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        Some("100-1".to_string()),
    );
    let emitted = serde_json::to_value(envelope).unwrap();
    let emitted_keys: BTreeSet<_> = emitted.as_object().unwrap().keys().cloned().collect();
    let property_keys: BTreeSet<_> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let required: BTreeSet<_> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect();

    assert_eq!(emitted_keys, property_keys);
    assert_eq!(emitted_keys, required);
    assert_object_shape(&emitted, &schema);
    assert_object_shape(&emitted["producer"], &schema["properties"]["producer"]);
    for variant in schema["$defs"]["provenance"]["oneOf"].as_array().unwrap() {
        if variant["additionalProperties"] == false {
            assert_eq!(
                keys(&variant["properties"]),
                required_set(variant),
                "provenance variant properties must be exactly required"
            );
        } else {
            assert_eq!(variant["additionalProperties"], true);
            assert_eq!(variant["required"], serde_json::json!(["kind"]));
            assert_eq!(
                variant["properties"]["kind"]["not"]["enum"],
                serde_json::json!(["git", "token", "artifact"])
            );
        }
    }
}

/// Pins the sauma canonical form and digest to a fixed vector so the digest law
/// is enforced by CI, not just described in prose. The schema `description`
/// promises "object keys sorted lexicographically at every level"; this proves
/// the code delivers exactly that and that the digest is a stable content-address
/// a foreign walker can recompute. If this vector changes, the schema description
/// and any cross-tool digest contract change with it — that is the point.
#[test]
fn sauma_canonical_digest_matches_pinned_vector() {
    // Keys deliberately out of lexical order, with a nested object and an array,
    // to prove sorting happens at every level while array order is preserved.
    let artifact = serde_json::json!({
        "b": 1,
        "a": { "y": true, "x": [3, 1, 2] },
        "c": "hi"
    });

    let canonical = toimija::sauma::canonical_json(&artifact);
    assert_eq!(
        canonical, r#"{"a":{"x":[3,1,2],"y":true},"b":1,"c":"hi"}"#,
        "canonical form must sort object keys at every level, compact, arrays in order"
    );

    assert_eq!(
        toimija::sauma::digest_value(&artifact),
        "sha256:adac1485cdabade54a2ab242128ff4ad18618ee9038acdcc6404ea9be057853d",
        "digest must be sha256 over the sorted-key canonical form"
    );
}

fn assert_object_shape(emitted: &serde_json::Value, schema: &serde_json::Value) {
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(keys(emitted), keys(&schema["properties"]));
    assert_eq!(keys(emitted), required_set(schema));
}

fn keys(value: &serde_json::Value) -> std::collections::BTreeSet<String> {
    value.as_object().unwrap().keys().cloned().collect()
}

fn required_set(schema: &serde_json::Value) -> std::collections::BTreeSet<String> {
    schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect()
}
