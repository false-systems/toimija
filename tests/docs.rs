//! Documentation guarantees. The Git-state thesis is part of the product, so
//! the README must state it verbatim.

const README: &str = include_str!("../README.md");
const HANDOFF_SCHEMA: &str = include_str!("../docs/schemas/handoff-record.v1.schema.json");

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
fn readme_has_a_git_starting_point_section() {
    assert!(
        README.contains("## Git as the starting point"),
        "README.md must keep the Git-as-the-starting-point section"
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
    assert_eq!(schema["$id"], "docs/schemas/handoff-record.v1.schema.json");
    assert_eq!(schema["additionalProperties"], false);

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
