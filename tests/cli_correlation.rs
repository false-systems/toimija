use std::fs;

use assert_cmd::Command;
use false_agent_protocol::{RepositoryPointEnvelope, WorkerRunResultEnvelope};

mod common;
use common::fixture::RepoFixture;

#[test]
fn correlate_pr_binds_a_clean_destination_to_bounded_work() {
    let fixture = RepoFixture::new();
    fixture.write("README.md", "demo\n");
    fixture.write(".gitignore", ".toimija/\n");
    fixture.commit("init");
    fixture.git(&[
        "remote",
        "add",
        "origin",
        "git@github.com:false-systems/demo.git",
    ]);

    let point_output = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(fixture.path())
        .args(["repository-point", "--json"])
        .output()
        .unwrap();
    assert!(point_output.status.success());
    let point: RepositoryPointEnvelope = serde_json::from_slice(&point_output.stdout).unwrap();

    let mut result: WorkerRunResultEnvelope =
        serde_json::from_str(false_agent_protocol::WORKER_RESULT_FIXTURE).unwrap();
    result.origin = point.repository.id.clone();
    result.destination = Some(point.repository.id.clone());
    let binding = result.binding.as_mut().unwrap();
    binding.origin = point.repository.id.clone();
    binding.work = "work_demo".parse().unwrap();
    binding.spec = "spec_demo".parse().unwrap();
    let evidence_digest = result.evidence[0].digest.clone();
    result.seal().unwrap();

    let runs = fixture.path().join(".toimija/runs");
    fs::create_dir_all(&runs).unwrap();
    fs::write(
        runs.join(format!("{}.result.json", result.run)),
        serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();

    let session = fixture.path().join(".toimija/sessions/100-1");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("session.json"),
        serde_json::to_vec(&serde_json::json!({
            "id": "100-1",
            "pid": 1,
            "agent": ["kisko"],
            "worktree": fixture.path().canonicalize().unwrap(),
            "branch": "main",
            "head": null,
            "intent": "demo",
            "scope": [],
            "started_at": 100,
            "last_seen_at": 100,
            "status": "closed",
            "closed_at": 101,
            "exit_status": 0
        }))
        .unwrap(),
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(fixture.path())
        .args([
            "correlate-pr",
            "https://github.com/false-systems/demo/pull/7",
            "--session",
            "100-1",
            "--run",
            result.run.as_str(),
        ])
        .assert()
        .success();

    let artifact = runs.join(format!("{}.correlation.json", result.run));
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&artifact).unwrap()).unwrap();
    assert_eq!(value["schema"], "work-correlation.v1");
    assert_eq!(value["spec"], "spec_demo");
    assert_eq!(value["work"], "work_demo");
    assert_eq!(value["pull_request"]["ref"], "github:false-systems/demo#7");
    assert_eq!(value["destination"], point.repository.id.as_str());
    assert!(value["created_at"].is_string());
    assert!(
        value["sauma"]["provenance"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edge| edge["kind"] == "artifact" && edge["digest"] == evidence_digest)
    );
    assert_eq!(toimija::sauma::verify_value(value.take()).unwrap(), None);
}
