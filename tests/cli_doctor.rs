use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

mod common;
use common::{git_available, run_git};

fn init_git_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "toimija@example.invalid"]);
    run_git(path, &["config", "user.name", "Toimija Test"]);
    fs::write(path.join("README.md"), "# Demo\n").unwrap();
    run_git(path, &["add", "README.md"]);
    run_git(path, &["commit", "-m", "initial"]);
}

fn write_ready_rust_repo(path: &Path) {
    fs::write(
        path.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(path.join("AGENTS.md"), "# Agent guide\n").unwrap();
    fs::write(path.join("CLAUDE.md"), "See AGENTS.md.\n").unwrap();
    fs::create_dir_all(path.join(".toimija")).unwrap();
    fs::write(
        path.join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test fixture\ncontext:\n  important_docs:\n    - README.md\nvalidation:\n  fast:\n    - cargo test --workspace\nboundaries:\n  - stay inside fixture\n",
    )
    .unwrap();
}

/// A repo whose agent docs honour the full contract. This is what `--strict`
/// requires, so it is its own fixture rather
/// than a tweak of `write_ready_rust_repo` (whose docs are intentionally minimal).
fn write_strict_ready_rust_repo(path: &Path) {
    fs::write(
        path.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        path.join("AGENTS.md"),
        "# Agent guide\n\nBefore editing, run `toimija context --for-diff`.\n\nStay inside fixture.\n",
    )
    .unwrap();
    fs::write(path.join("CLAUDE.md"), "See AGENTS.md.\n").unwrap();
    fs::create_dir_all(path.join(".toimija")).unwrap();
    fs::write(
        path.join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test fixture\ncontext:\n  important_docs:\n    - README.md\nvalidation:\n  fast:\n    - cargo test --workspace\nboundaries:\n  - Stay inside fixture.\n",
    )
    .unwrap();
    // Git ignores what a refresh writes, as `toimija init` arranges.
    fs::write(
        path.join(".toimija/.gitignore"),
        "/current.md\n/current.json\n/sessions/\n/history.jsonl\n/history.lock\n/runs/\n/snapshots/\n",
    )
    .unwrap();
}

#[test]
fn doctor_outside_git_repo_reports_failure() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains("fail: inside git repo"));
}

#[test]
fn doctor_ready_rust_repo_succeeds() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_ready_rust_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: toimija config - config found",
        ))
        .stdout(predicate::str::contains(
            "ok: repo role - repo role present",
        ))
        .stdout(predicate::str::contains(
            "ok: repo kind - Rust repo detected",
        ))
        .stdout(predicate::str::contains(
            "ok: validation plan - validation configured",
        ))
        .stdout(predicate::str::contains(
            "ok: CLAUDE.md - CLAUDE.md references AGENTS.md",
        ));
}

#[test]
fn doctor_missing_config_is_advisory() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::write(temp.path().join("AGENTS.md"), "# Agent guide\n").unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "warn: toimija config - missing config; run `toimija init`",
        ))
        .stdout(predicate::str::contains(
            "warn: repo role - repo role is empty",
        ))
        .stdout(predicate::str::contains(
            "warn: boundaries - boundaries are empty",
        ));
}

#[test]
fn doctor_invalid_config_fails() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(temp.path().join(".toimija/repo.yaml"), "repo: [").unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "fail: toimija config - invalid repo config",
        ));
}

#[test]
fn doctor_rejects_zero_stewardship_retention() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "stewardship:\n  runs_retention_days: 0\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains("runs_retention_days"))
        .stdout(predicate::str::contains("must be between 1 and"));
}

#[test]
fn doctor_reports_drift_as_advisory_by_default() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    // Minimal docs: no preflight mention, boundary not mirrored. Plain doctor
    // surfaces these as warnings but still exits 0.
    write_ready_rust_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "warn: preflight mention - no agent doc mentions the `toimija enter` preflight",
        ))
        .stdout(predicate::str::contains("warn: boundaries mirrored -"));
}

#[test]
fn doctor_boundaries_satisfied_by_repo_yaml_reference() {
    // Boundaries need not be mirrored verbatim: an agent doc that points at
    // `.toimija/repo.yaml` is single-sourced and therefore drift-free, so the
    // check passes without duplicating the boundary text.
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_ready_rust_repo(temp.path());
    fs::write(
        temp.path().join("AGENTS.md"),
        "# Agent guide\n\nBoundaries live in `.toimija/repo.yaml`; run `toimija context` to load them.\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: boundaries mirrored - boundaries sourced by reference to .toimija/repo.yaml",
        ));
}

#[test]
fn doctor_strict_passes_on_this_repo() {
    // Dogfood: the README tells people to run `toimija doctor --strict` as the
    // PR/CI gate, so this repo must pass its own gate. Guards against boundary
    // wording in .toimija/repo.yaml drifting from AGENTS.md/CLAUDE.md.
    if !git_available() {
        return;
    }
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["doctor", "--strict"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: boundaries mirrored - boundaries appear in the agent docs",
        ));
}

#[test]
fn doctor_strict_ready_repo_succeeds() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_strict_ready_rust_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["doctor", "--strict"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Toimija doctor (strict)"))
        .stdout(predicate::str::contains(
            "ok: preflight mention - agent docs point at the `toimija enter` preflight",
        ))
        .stdout(predicate::str::contains(
            "ok: boundaries mirrored - boundaries appear in the agent docs",
        ));
}

#[test]
fn doctor_strict_escalates_drift_to_failure() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    // Same minimal-docs repo that only warns under plain doctor.
    write_ready_rust_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["doctor", "--strict"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "fail: preflight mention - no agent doc mentions the `toimija enter` preflight",
        ))
        .stdout(predicate::str::contains("fail: boundaries mirrored -"));
}

#[test]
fn doctor_accepts_boundaries_sourced_by_reference() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    // The agent docs point at the manifest instead of mirroring the boundary
    // verbatim — a single source of truth, so doctor must accept it rather than
    // demand duplication.
    fs::write(
        temp.path().join("AGENTS.md"),
        "# Agent guide\nBoundaries are defined in .toimija/repo.yaml; read them there.\n",
    )
    .unwrap();
    fs::write(temp.path().join("CLAUDE.md"), "See AGENTS.md.\n").unwrap();
    fs::create_dir_all(temp.path().join(".toimija")).unwrap();
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test fixture\nboundaries:\n  - a very specific boundary line not copied into the docs\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: boundaries mirrored - boundaries sourced by reference to .toimija/repo.yaml",
        ));
}

#[test]
fn doctor_strict_flags_missing_important_doc() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_strict_ready_rust_repo(temp.path());
    // Nominate a doc that does not exist on disk.
    fs::write(
        temp.path().join(".toimija/repo.yaml"),
        "repo:\n  name: demo\n  role: test fixture\ncontext:\n  important_docs:\n    - README.md\n    - docs/MISSING.md\nboundaries:\n  - Stay inside fixture.\n",
    )
    .unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .args(["doctor", "--strict"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "fail: important docs - missing on disk: docs/MISSING.md",
        ));
}

#[test]
fn doctor_claude_without_agents_reference_is_advisory() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_ready_rust_repo(temp.path());
    fs::write(temp.path().join("CLAUDE.md"), "Local Claude notes.\n").unwrap();

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "warn: CLAUDE.md - CLAUDE.md exists but does not reference AGENTS.md",
        ));
}

#[test]
fn doctor_warns_when_git_would_list_the_packet() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_ready_rust_repo(temp.path());

    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "warn: packet ignored - Git does not ignore .toimija/current.md, .toimija/current.json, .toimija/sessions/",
        ))
        .stdout(predicate::str::contains("toimija init"));

    // The rules may live anywhere Git reads them; here, the repo's own.
    fs::write(
        temp.path().join(".gitignore"),
        ".toimija/*\n!.toimija/repo.yaml\n",
    )
    .unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: packet ignored - Git ignores the packet, sessions, history, runs and snapshots",
        ));
}

#[test]
fn doctor_checks_every_directory_init_ignores() {
    if !git_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    init_git_repo(temp.path());
    write_ready_rust_repo(temp.path());
    // The exact-name rules most repos carry: no `runs/`, no `snapshots/`.
    // `snapshot --lease` and harness transcripts would then show up as
    // untracked work, which a pre-commit hook refuses.
    fs::write(
        temp.path().join(".gitignore"),
        ".toimija/current.md\n.toimija/current.json\n.toimija/sessions/\n.toimija/history.jsonl\n.toimija/history.lock\n",
    )
    .unwrap();
    Command::cargo_bin("toimija")
        .unwrap()
        .current_dir(temp.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "warn: packet ignored - Git does not ignore .toimija/runs/, .toimija/snapshots/;",
        ));
}
