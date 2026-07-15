//! Sykli CI contract for toimija: the same strict gate the repo's own
//! validation plan renders, written as a typed execution graph instead of
//! workflow YAML.
//!
//! Local run: `sykli run` at the repo root. The sykli CLI compiles this file
//! via `cargo run --features sykli --bin sykli -- --emit`, so ordinary
//! `cargo build`/`test`/`clippy` never pay for the SDK (the `sykli` feature
//! is off by default).
//!
//! Scope note: this is Sykli **as this repo's CI** — the pipeline definition
//! only. Sykli delegation inside toimija's own commands remains out of scope
//! per the boundaries in `.toimija/repo.yaml`.

use sykli::Pipeline;

fn main() {
    let mut p = Pipeline::new();

    // The three gates AGENTS.md demands before claiming completion.
    let _ = p.task("fmt").run("cargo fmt --check");
    let _ = p
        .task("clippy")
        .run("cargo clippy --all-targets -- -D warnings");
    let _ = p.task("test").run("cargo test");

    // Dogfood: the repo must pass its own drift guard. After `test` so the
    // debug binary is already built and this stays cheap.
    let _ = p
        .task("doctor")
        .run("cargo run --quiet -- doctor --strict")
        .after(&["test"]);

    p.emit();
}
