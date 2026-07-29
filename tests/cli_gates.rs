use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use assert_cmd::Command;
use false_agent_protocol::{
    CompletionLevel, GateRequirementSummary, ObligationKind, ObligationRef, ObligationState,
    ObligationSummary, ProtocolVersion, ScopeSummary, SpecSummary, WorkPacketEnvelope, WorkSummary,
    WorkflowStatusSummary,
};
use predicates::prelude::*;

mod common;
use common::{git_available, run_git};

fn init(path: &Path) {
    run_git(path, &["init", "-b", "main"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::create_dir_all(path.join("tools")).unwrap();
    fs::write(path.join(".gitignore"), "target/\n").unwrap();
    fs::write(path.join("tracked.txt"), "base\n").unwrap();
    write_executable(
        &path.join("tools/pass"),
        "#!/bin/sh\nprintf 'standard output\\n'\nprintf 'standard error\\n' >&2\n",
    );
    write_executable(
        &path.join("tools/mutate"),
        "#!/bin/sh\nprintf 'mutated\\n' > tracked.txt\n",
    );
    write_executable(
        &path.join("tools/generated"),
        "#!/bin/sh\nmkdir -p target\nprintf 'generated\\n' > target/output\n",
    );
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "fixture"]);
    write_config(path, "");
}

fn write_config(path: &Path, pass_args: &str) {
    fs::create_dir_all(path.join(".toimija")).unwrap();
    fs::write(
        path.join(".toimija/repo.yaml"),
        format!(
            "repo:\n  name: fixture\n\
             gates:\n\
             \x20 pass:\n\
             \x20   description: passing gate\n\
             \x20   program: ./tools/pass\n\
             \x20   args: [{pass_args}]\n\
             \x20 mutate:\n\
             \x20   description: mutating gate\n\
             \x20   program: ./tools/mutate\n\
             \x20 generated:\n\
             \x20   description: generated output gate\n\
             \x20   program: ./tools/generated\n\
             gate_groups:\n\
             \x20 full:\n\
             \x20   gates: [pass, generated]\n"
        ),
    )
    .unwrap();
}

fn write_executable(path: &Path, source: &str) {
    fs::write(path, source).unwrap();
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn frame(value: serde_json::Value) -> String {
    let body = serde_json::to_string(&value).unwrap();
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

fn fake_work_frames() -> String {
    let obligation: ObligationRef = "obl_gate-check".parse().unwrap();
    let packet = WorkPacketEnvelope {
        protocol: ProtocolVersion::V1,
        spec: SpecSummary {
            id: "spec_fixture".parse().unwrap(),
            title: "Gate fixture".into(),
            objective: "verify the current tree".into(),
            goals: Vec::new(),
            non_goals: Vec::new(),
            source_revision: None,
        },
        work: WorkSummary {
            id: "work_fixture".parse().unwrap(),
            spec: "spec_fixture".parse().unwrap(),
            objective: "verify the current tree".into(),
        },
        scope: ScopeSummary {
            write: vec!["tracked.txt".into()],
        },
        obligations: vec![ObligationSummary {
            id: obligation.clone(),
            description: "check is current".into(),
            kind: ObligationKind::Gate,
        }],
        gates: vec![GateRequirementSummary {
            id: "gate_check".parse().unwrap(),
            obligation: obligation.clone(),
            command: String::new(),
        }],
        required_completion: CompletionLevel::Validated,
        extensions: [(
            "toimija.required_gates.v1".into(),
            serde_json::json!(["check"]),
        )]
        .into(),
    };
    let status = |supported, closable| WorkflowStatusSummary {
        obligations: vec![ObligationState {
            obligation: obligation.clone(),
            supported,
            evidence: Vec::new(),
        }],
        blockers: if closable {
            Vec::new()
        } else {
            vec![false_agent_protocol::BlockerSummary {
                code: "obligation-gate-check".into(),
                message: "gate check has no current Toimija receipt".into(),
            }]
        },
        closable,
    };
    [
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {
                "plugin": {"id": "teko", "name": "fake", "version": "1", "kind": "work"},
                "selected_protocol": {"major": 1, "minor": 0},
                "capabilities": [{
                    "id": "work.v1",
                    "methods": ["work.current", "work.explain", "work.observe_gates"]
                }],
                "limits": false_agent_protocol::plugin::PluginLimits::default()
            }
        }),
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "result": {"state": "active", "work": packet.clone()}
        }),
        serde_json::json!({
            "jsonrpc": "2.0", "id": 3,
            "result": {
                "work": "work_fixture",
                "status": status(false, false),
                "explanation": "missing receipt"
            }
        }),
        serde_json::json!({
            "jsonrpc": "2.0", "id": 4,
            "result": {"observed": 1}
        }),
        serde_json::json!({
            "jsonrpc": "2.0", "id": 5,
            "result": {"state": "active", "work": packet}
        }),
        serde_json::json!({
            "jsonrpc": "2.0", "id": 6,
            "result": {
                "work": "work_fixture",
                "status": status(true, true),
                "explanation": "ready"
            }
        }),
        serde_json::json!({"jsonrpc": "2.0", "id": 7, "result": {}}),
    ]
    .into_iter()
    .map(frame)
    .collect()
}

#[test]
fn list_inspect_run_status_and_staleness_are_machine_readable() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["gates", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pass"))
        .stdout(predicate::str::contains("group: pass, generated"));

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["gates", "inspect", "pass", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"program\": \"./tools/pass\""))
        .stdout(predicate::str::contains("\"spec_digest\": \"sha256:"));

    let run = Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["gates", "run", "pass", "--json"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let run: serde_json::Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(run["current"], true);
    assert_eq!(run["gates"][0]["outcome"], "passed");
    assert_eq!(run["gates"][0]["receipt"]["stdout"]["bytes_observed"], 16);
    assert_eq!(run["gates"][0]["receipt"]["stderr"]["bytes_observed"], 15);

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["gates", "status", "--gate", "pass", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"current\""));

    fs::write(temp.path().join("tracked.txt"), "changed\n").unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["gates", "status", "--gate", "pass", "--json"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("\"status\": \"stale tree\""));

    write_config(temp.path(), "\"changed-definition\"");
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["gates", "status", "--gate", "pass", "--json"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "\"status\": \"gate definition changed\"",
        ));
}

#[test]
fn source_mutation_never_mints_a_receipt_but_ignored_output_can() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["gates", "run", "mutate", "--json"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("\"outcome\": \"subject_mutated\""))
        .stdout(predicate::str::contains("\"receipt\": null"));

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["gates", "run", "generated", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"outcome\": \"passed\""))
        .stdout(predicate::str::contains("\"id\": \"gatercpt_"));

    assert!(!temp.path().join("target").exists());
    assert_eq!(
        fs::read_to_string(temp.path().join("tracked.txt")).unwrap(),
        "base\n"
    );
}

#[test]
fn shell_executables_and_unknown_gate_ids_fail_closed() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "gates:\n  unsafe:\n    program: sh\n    args: [-c, echo model text]\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["gates", "run", "unsafe"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("is a shell"));

    write_config(temp.path(), "");
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["gates", "run", "unknown"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown gate"));
}

#[test]
fn verify_work_runs_required_ids_imports_receipts_and_refreshes_teko() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    write_config(temp.path(), "");
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: fixture\n\
         composition:\n  work: teko\n  require_work: true\n\
         gates:\n  check:\n    description: exact fixture check\n    program: /usr/bin/true\n",
    )
    .unwrap();

    let config = tempfile::tempdir().unwrap();
    fs::create_dir_all(config.path().join("toimija")).unwrap();
    fs::write(
        config.path().join("toimija/plugins.toml"),
        "[plugins.teko]\n\
         kind = \"work\"\n\
         command = \"/bin/sh\"\n\
         args = [\"-c\", \"printf %s \\\"$FAKE_FRAMES\\\"; sleep 30\"]\n\
         timeout_ms = 5000\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .env("XDG_CONFIG_HOME", config.path())
        .env("FAKE_FRAMES", fake_work_frames())
        .args(["verify-work", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"outcome\": \"passed\""))
        .stdout(predicate::str::contains("\"closable\": true"))
        .stdout(predicate::str::contains("\"id\": \"gatercpt_"));
}
