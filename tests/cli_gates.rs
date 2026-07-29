use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use assert_cmd::Command;
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
