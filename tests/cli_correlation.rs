use std::fs;

use assert_cmd::Command;
use false_agent_protocol::{RepositoryPointEnvelope, WorkerRunResultEnvelope};

mod common;
use common::fixture::RepoFixture;

fn bounded_fixture() -> (RepoFixture, WorkerRunResultEnvelope) {
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
    result.seal().unwrap();

    let runs = fixture.path().join(".toimija/runs");
    fs::create_dir_all(&runs).unwrap();
    fs::write(
        runs.join(format!("{}.result.json", result.run)),
        serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
    (fixture, result)
}

fn finalized_session(fixture: &RepoFixture, result: &WorkerRunResultEnvelope) -> String {
    let mut session = toimija::session::Session::open(
        fixture.path(),
        &["kisko".into()],
        Some("demo".into()),
        Vec::new(),
    )
    .unwrap();
    session.close(Some(0)).unwrap();
    session.record_worker_run(fixture.path(), result).unwrap();
    session.id
}

fn edit_marker(fixture: &RepoFixture, session: &str, key: &str, value: serde_json::Value) {
    let path = fixture
        .path()
        .join(".toimija/sessions")
        .join(session)
        .join("session.json");
    let mut marker: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    marker[key] = value;
    fs::write(path, serde_json::to_vec(&marker).unwrap()).unwrap();
}

#[test]
fn correlate_pr_derives_the_session_that_produced_the_run() {
    let (fixture, result) = bounded_fixture();
    let session = finalized_session(&fixture, &result);

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(fixture.path())
        .args([
            "correlate-pr",
            "https://github.com/false-systems/demo/pull/7",
            "--run",
            result.run.as_str(),
        ])
        .assert()
        .success();

    let artifact = fixture
        .path()
        .join(".toimija/runs")
        .join(format!("{}.correlation.json", result.run));
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&artifact).unwrap()).unwrap();
    assert_eq!(value["schema"], "work-correlation.v1");
    assert_eq!(value["spec"], "spec_demo");
    assert_eq!(value["work"], "work_demo");
    assert_eq!(value["session"], session);
    assert_eq!(value["pull_request"]["ref"], "github:false-systems/demo#7");
    assert_eq!(
        value["destination"],
        result.destination.as_ref().unwrap().as_str()
    );
    assert!(value["created_at"].is_string());
    assert!(
        value["sauma"]["provenance"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edge| edge["kind"] == "artifact" && edge["digest"] == result.evidence[0].digest)
    );
    assert_eq!(toimija::sauma::verify_value(value.take()).unwrap(), None);
}

#[test]
fn correlate_pr_rejects_a_session_not_linked_to_the_run() {
    let (fixture, result) = bounded_fixture();
    let session = finalized_session(&fixture, &result);

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
        .failure()
        .stderr(predicates::str::contains(format!(
            "produced by session {session}, not 100-1"
        )));
}

#[test]
fn correlate_pr_rejects_an_active_producing_session() {
    let (fixture, result) = bounded_fixture();
    let session = finalized_session(&fixture, &result);
    edit_marker(&fixture, &session, "status", serde_json::json!("active"));

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(fixture.path())
        .args([
            "correlate-pr",
            "https://github.com/false-systems/demo/pull/7",
            "--run",
            result.run.as_str(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(format!(
            "session {session} is still active"
        )));
}

#[test]
fn correlate_pr_rejects_a_mismatched_session_marker_id() {
    let (fixture, result) = bounded_fixture();
    let session = finalized_session(&fixture, &result);
    edit_marker(&fixture, &session, "id", serde_json::json!("100-2"));

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(fixture.path())
        .args([
            "correlate-pr",
            "https://github.com/false-systems/demo/pull/7",
            "--run",
            result.run.as_str(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(format!(
            "session marker id does not match `{session}`"
        )));
}

#[test]
fn correlate_pr_keeps_run_provenance_after_session_cleanup() {
    let (fixture, result) = bounded_fixture();
    let session = finalized_session(&fixture, &result);
    fs::remove_file(
        fixture
            .path()
            .join(".toimija/sessions")
            .join(session)
            .join("session.json"),
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(fixture.path())
        .args([
            "correlate-pr",
            "https://github.com/false-systems/demo/pull/7",
            "--run",
            result.run.as_str(),
        ])
        .assert()
        .success();
}
