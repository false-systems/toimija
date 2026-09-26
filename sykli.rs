//! Sykli CI contract for toimija (sykli-contract.v1): the same strict gate the
//! repo's own validation plan renders, as a declared work graph instead of
//! workflow YAML. Receipts are content-addressed, so a task whose inputs have
//! not changed is a cache hit.
//!
//! Local run: `sykli run` at the repo root. Sykli compiles this file via
//! `cargo run --features sykli --bin sykli -- --emit`; the `sykli` feature is
//! off by default and pulls in nothing, so ordinary builds never build it.
//!
//! Scope note: this is Sykli **as this repo's CI** — the pipeline definition
//! only. Sykli delegation inside toimija's own commands remains out of scope
//! per the boundaries in `.toimija/repo.yaml`.

use serde_json::json;
use std::{io, path::Path};

/// The tracked files, minus the lock that pins this contract (it would change
/// the contract it pins). Tracked only: a scratch file must not change the
/// contract, and an untracked source file is still caught — `sykli run`
/// refuses Cargo sources missing from every task's inputs.
fn repository_files(root: &Path) -> io::Result<Vec<String>> {
    let out = std::process::Command::new("git")
        .args(["ls-files", "--", ".", ":(exclude)sykli.lock"])
        .current_dir(root)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "git ls-files: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let mut files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    files.sort();
    Ok(files)
}

fn main() {
    if !std::env::args().any(|a| a == "--emit") {
        eprintln!("invoke this contract through `sykli run`");
        std::process::exit(2);
    }
    let files = repository_files(Path::new(".")).expect("read repository files");
    // What the compiler and the linters read.
    let code: Vec<_> = files
        .iter()
        .filter(|p| {
            p.ends_with(".rs")
                || p.ends_with("Cargo.toml")
                || p.as_str() == "Cargo.lock"
                || p.as_str() == "rust-toolchain.toml"
                || p.starts_with(".cargo/")
        })
        .cloned()
        .collect();
    let fmt_inputs: Vec<_> = code
        .iter()
        .filter(|p| p.ends_with(".rs") || p.ends_with("Cargo.toml"))
        .cloned()
        .collect();
    // The tests and the drift guard read far more than code — fixtures, the
    // agent docs, repo.yaml, the README — so they depend on every file. An
    // undeclared input would be served from cache.
    let contract = json!({
        "schema": "sykli-contract.v1",
        "tasks": [
            // The three gates AGENTS.md demands before claiming completion.
            { "name": "fmt", "run": "cargo fmt --check", "inputs": fmt_inputs },
            { "name": "clippy",
              "run": "cargo clippy --all-targets -- -D warnings",
              "after": ["fmt"], "inputs": code },
            { "name": "test", "run": "cargo test", "after": ["fmt"], "inputs": files.clone() },
            // Dogfood: the repo must pass its own drift guard. After `test`
            // so the debug binary is already built and this stays cheap.
            { "name": "doctor",
              "run": "cargo run --quiet -- doctor --strict",
              "after": ["test"], "inputs": files }
        ]
    });
    serde_json::to_writer(std::io::stdout(), &contract).expect("write contract");
    println!();
}
