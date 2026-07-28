//! `xtask` must fail closed: an unknown task is an error, not a help screen.
//! Callers treat the exit code as a verdict — the harness ran
//! `cargo xtask gate` before the task existed, read the help screen's exit 0,
//! and recorded a passing gate that had run nothing.

use std::process::Command;

#[test]
fn unknown_task_exits_nonzero_and_names_the_task() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("definitely-not-a-task")
        .output()
        .unwrap();
    assert!(!output.status.success(), "unknown task must not exit 0");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("definitely-not-a-task"), "stderr: {stderr}");
}

#[test]
fn bare_invocation_prints_the_task_list_and_succeeds() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask")).output().unwrap();
    assert!(
        output.status.success(),
        "bare `cargo xtask` is a help screen"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cargo xtask gate"), "stderr: {stderr}");
}
