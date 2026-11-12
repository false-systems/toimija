//! The sykli contract serves a task from cache while its declared inputs are
//! unchanged. A file a compiled target reads with `include_str!` is an input
//! of every task that compiles it — `clippy --all-targets` as much as `test`
//! — so it must be declared there, or an edit to it is served a stale pass.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// The tasks that compile every target: what they read includes every
/// `include_str!`.
const COMPILING: [&str; 2] = ["clippy", "test"];

#[test]
fn every_included_file_is_an_input_of_the_tasks_that_compile_it() {
    let root = repository_root();
    let contract: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("sykli.json")).unwrap()).unwrap();
    let included = included_files(&root);
    // Positive control: tests/docs.rs reads the README.
    assert!(
        included.contains("README.md"),
        "the scan found no include_str! of README.md: {included:?}"
    );

    let mut missing = Vec::new();
    for task in COMPILING {
        let inputs: BTreeSet<&str> = contract["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|candidate| candidate["name"] == task)
            .unwrap_or_else(|| panic!("no task {task} in sykli.json"))["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|input| input.as_str().unwrap())
            .collect();
        for path in &included {
            if !inputs.contains(path.as_str()) {
                missing.push(format!("{task}: {path}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "read with include_str! but not a declared input (add it, then `sykli lock sykli.json`):\n{}",
        missing.join("\n")
    );
}

/// The repository's root: the nearest folder at or above the current one
/// that holds `sykli.json`. Cargo runs integration tests from the package.
fn repository_root() -> PathBuf {
    let start = std::env::current_dir().unwrap();
    start
        .ancestors()
        .find(|folder| folder.join("sykli.json").is_file())
        .unwrap_or_else(|| panic!("no sykli.json at or above {}", start.display()))
        .to_path_buf()
}

/// Every path an `include_str!` in a `.rs` file under `src/`, `tests/` or
/// `xtask/` names, relative to `root`.
fn included_files(root: &Path) -> BTreeSet<String> {
    let mut sources = Vec::new();
    for dir in ["src", "tests", "xtask"] {
        rust_files(&root.join(dir), &mut sources);
    }
    let mut included = BTreeSet::new();
    for source in sources {
        let text = fs::read_to_string(&source).unwrap();
        for (at, _) in text.match_indices("include_str!(\"") {
            let rest = &text[at + "include_str!(\"".len()..];
            let Some(end) = rest.find('"') else {
                continue;
            };
            let path = source.parent().unwrap().join(&rest[..end]);
            included.insert(relative(root, &path));
        }
    }
    included
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// `path` relative to `root`, with `..` resolved by name, as sykli.json
/// spells its inputs.
fn relative(root: &Path, path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for component in path.strip_prefix(root).unwrap().components() {
        match component {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    parts.join("/")
}

#[test]
fn a_relative_include_resolves_to_the_contract_spelling() {
    let root = Path::new("/repo");
    assert_eq!(
        relative(root, Path::new("/repo/tests/../docs/reference.md")),
        "docs/reference.md"
    );
    assert_eq!(relative(root, Path::new("/repo/README.md")), "README.md");
}
