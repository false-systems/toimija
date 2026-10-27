//! Outputs unchanged: this build against a baseline toimija, command by
//! command, on repositories in each state a packet tells apart
//! (`common::scenarios`). Each binary runs on its own fresh build of a
//! scenario at the same path, with the same environment, and every step's exit
//! code, stdout, stderr and the packets it leaves behind must be equal. Masked,
//! and nothing else: the moments a packet or a point was made, the ids made
//! from those moments, the digests over them, and the session id.
//!
//! It needs the baseline binary, so it is ignored by default:
//!
//! ```sh
//! git worktree add /tmp/toimija-main origin/main
//! cargo build --release --manifest-path /tmp/toimija-main/Cargo.toml
//! TOIMIJA_BASELINE=/tmp/toimija-main/target/release/toimija \
//!     cargo test --test differential -- --ignored --nocapture
//! ```
//!
//! `TOIMIJA_CANDIDATE` replaces this build with another binary: a baseline
//! against itself shows the masks hide only what varies from run to run.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;
use common::git_available;
use common::scenarios::{self, HERMETIC_GIT_ENV};

/// Keys whose values differ between two runs of one binary on one state.
const MASKED_KEYS: [&str; 12] = [
    "artifact_id",
    "captured_at",
    "generated_at",
    "generation",
    "context_digest",
    "digest",
    "artifact_digest",
    "started_at",
    "last_seen_at",
    "pid",
    "pid_start",
    "closed_at",
];

/// The wrapped command of the first `run` step: it keeps the packet it was
/// launched with, then changes the worktree, so the exit refresh has something
/// to see while the refs stay where they were.
const RUN_SCRIPT: &str = "cp \"$TOIMIJA_LIVE_JSON\" \"$OUT/during.json\" && \
                          echo exit >> late.txt";

/// The second: it moves HEAD and no ref, so the refs' stamp holds while what
/// was read from HEAD — the merge-base, the branch diff, the recent commits —
/// does not, and the exit refresh must read it again.
const RUN_DETACHING: &str = "git checkout -q --detach HEAD~1 || true";

#[derive(Debug, Clone, Copy)]
enum Step {
    Toimija(&'static [&'static str]),
    /// A new untracked file, made between two toimija steps.
    Edit,
}

const STEPS: [Step; 19] = [
    Step::Toimija(&["validate", "--plan"]),
    Step::Toimija(&["context", "--json"]),
    Step::Toimija(&["context"]),
    Step::Toimija(&["context", "--for-diff", "--budget", "4000"]),
    Step::Toimija(&["landed", "--json"]),
    Step::Toimija(&["repository-point", "--json"]),
    Step::Toimija(&["snapshot", "--json"]),
    Step::Toimija(&["enter", "--json"]),
    Step::Toimija(&["verify", "--json"]),
    Step::Toimija(&["enter"]),
    Step::Toimija(&["refresh", "--json"]),
    Step::Toimija(&["sessions", "--json"]),
    Step::Edit,
    Step::Toimija(&["verify", "--json"]),
    Step::Toimija(&["run", "--intent", "diff", "--", "sh", "-c", RUN_SCRIPT]),
    Step::Toimija(&["sessions", "--json"]),
    Step::Toimija(&["run", "--intent", "detach", "--", "sh", "-c", RUN_DETACHING]),
    Step::Toimija(&["context", "--json"]),
    Step::Toimija(&["sessions", "--json"]),
];

/// What one step printed and left behind, masked.
#[derive(Debug, PartialEq, Eq)]
struct Observed {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    files: BTreeMap<String, String>,
}

fn mask_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if MASKED_KEYS.contains(&key.as_str()) {
                    *value = serde_json::Value::String("<masked>".to_string());
                } else {
                    mask_json(value);
                }
            }
            // Two sessions opened in one second are listed in the order of
            // their ids' pids, which differs from run to run: compare the list
            // as a set.
            if let Some(serde_json::Value::Array(sessions)) = map.get_mut("sessions") {
                sessions.sort_by_key(|session| session.to_string());
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(mask_json),
        _ => {}
    }
}

/// JSON with [`MASKED_KEYS`] masked; text with the packet's artifact-id line
/// and `run`'s refresh count masked (the watch may or may not see the wrapped
/// command's last write before it stops); each session id, in the order the
/// sessions were opened, replaced everywhere by its place in that order.
fn mask(text: &str, sessions: &[String]) -> String {
    let mut text = text.to_string();
    for (place, id) in sessions.iter().enumerate() {
        text = text.replace(id.as_str(), &format!("<session {}>", place + 1));
    }
    if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&text) {
        mask_json(&mut value);
        return serde_json::to_string_pretty(&value).unwrap();
    }
    text.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("- artifact-id:") {
                "- artifact-id: <masked>"
            } else if trimmed.starts_with("refreshed:") {
                "  refreshed: <masked>"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where two observations part: per field, the first line that differs.
fn first_differences(before: &Observed, after: &Observed) -> String {
    let mut fields: Vec<(String, &str, &str)> = vec![
        ("stdout".into(), &before.stdout, &after.stdout),
        ("stderr".into(), &before.stderr, &after.stderr),
    ];
    let names: std::collections::BTreeSet<&String> =
        before.files.keys().chain(after.files.keys()).collect();
    for name in names {
        fields.push((
            name.clone(),
            before.files.get(name).map_or("<absent>", String::as_str),
            after.files.get(name).map_or("<absent>", String::as_str),
        ));
    }
    let mut out = String::new();
    if before.code != after.code {
        out.push_str(&format!("  exit: {:?} -> {:?}\n", before.code, after.code));
    }
    for (name, before, after) in fields {
        if before == after {
            continue;
        }
        let mut lines = before.lines().zip(after.lines()).enumerate();
        let (line, (old, new)) = lines.find(|(_, (old, new))| old != new).unwrap_or((
            before.lines().count().min(after.lines().count()),
            ("<end>", "<end>"),
        ));
        out.push_str(&format!(
            "  {name}, line {}:\n    baseline:   {old}\n    this build: {new}\n",
            line + 1
        ));
    }
    out
}

fn toplevel(cwd: &Path) -> PathBuf {
    let output = common::git_at(cwd)
        .envs(HERMETIC_GIT_ENV)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .unwrap();
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

/// Run every step with `binary` on a fresh build of `scenario` at `base`.
fn observe(binary: &Path, scenario: &str, base: &Path) -> Vec<Observed> {
    if base.exists() {
        fs::remove_dir_all(base).unwrap();
    }
    let work = base.join("work");
    let home = base.join("home");
    let out = base.join("out");
    for dir in [&work, &home, &out] {
        fs::create_dir_all(dir).unwrap();
    }
    let cwd = scenarios::build(scenario, &work);
    let root = toplevel(&cwd);
    let mut sessions: Vec<String> = Vec::new();
    let mut observed = Vec::new();
    for step in STEPS {
        let args = match step {
            Step::Edit => {
                fs::write(cwd.join("late.txt"), "late\n").unwrap();
                continue;
            }
            Step::Toimija(args) => args,
        };
        let mut command = Command::new(binary);
        command
            .args(["-q", "--no-color"])
            .args(args)
            .current_dir(&cwd)
            .envs(HERMETIC_GIT_ENV)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("state"))
            .env("TOIMIJA_STATE_DIR", home.join("toimija"))
            .env("OUT", &out)
            .stdin(Stdio::null());
        for var in [
            "GIT_DIR",
            "GIT_INDEX_FILE",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "TOIMIJA_SESSION",
            "TOIMIJA_RUN",
            "TOIMIJA_LIVE",
            "TOIMIJA_LIVE_JSON",
        ] {
            command.env_remove(var);
        }
        let _ = fs::remove_file(out.join("during.json"));
        let output = command.output().unwrap();
        let mut opened = None;
        if args[0] == "run" {
            // The one session the step opened, launched or refused.
            opened = fs::read_dir(root.join(".toimija/sessions"))
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .find(|id| !sessions.contains(id));
            sessions.extend(opened.clone());
        }
        let mut files = BTreeMap::new();
        let mut keep = |name: String, path: PathBuf| {
            if let Ok(text) = fs::read_to_string(&path) {
                files.insert(name, mask(&text, &sessions));
            }
        };
        if matches!(args[0], "enter" | "refresh" | "run") {
            keep("current.json".into(), root.join(".toimija/current.json"));
            keep("current.md".into(), root.join(".toimija/current.md"));
        }
        if args[0] == "run" {
            keep("during.json".into(), out.join("during.json"));
            if let Some(id) = &opened {
                let dir = root.join(".toimija/sessions").join(id);
                keep("session current.json".into(), dir.join("current.json"));
                keep("session current.md".into(), dir.join("current.md"));
            }
        }
        observed.push(Observed {
            code: output.status.code(),
            stdout: mask(&String::from_utf8_lossy(&output.stdout), &sessions),
            stderr: mask(&String::from_utf8_lossy(&output.stderr), &sessions),
            files,
        });
    }
    observed
}

#[test]
#[ignore = "needs a baseline toimija: TOIMIJA_BASELINE=/path/to/toimija"]
fn every_output_is_the_baselines() {
    if !git_available() {
        return;
    }
    let baseline = PathBuf::from(
        std::env::var_os("TOIMIJA_BASELINE")
            .expect("set TOIMIJA_BASELINE to the toimija binary to compare against"),
    );
    let candidate = std::env::var_os("TOIMIJA_CANDIDATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_toimija")));
    let temp = tempfile::tempdir().unwrap();
    // The canonical spelling, so `/var` and `/private/var` never differ.
    let base = temp.path().canonicalize().unwrap().join("s");
    let mut compared = 0;
    let mut differences = Vec::new();
    for scenario in scenarios::ALL {
        let before = observe(&baseline, scenario, &base);
        let after = observe(&candidate, scenario, &base);
        let steps = STEPS.iter().filter_map(|step| match step {
            Step::Toimija(args) => Some(args.join(" ")),
            Step::Edit => None,
        });
        for ((step, before), after) in steps.zip(&before).zip(&after) {
            compared += 1;
            if before != after {
                differences.push(format!(
                    "{scenario}: toimija {step}\n{}",
                    first_differences(before, after)
                ));
            }
        }
    }
    eprintln!(
        "differential: {compared} steps over {} scenarios, {} differ",
        scenarios::ALL.len(),
        differences.len()
    );
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// The commands whose git processes the report counts, in the order it runs
/// them on one build of a scenario.
const COUNTED: [&[&str]; 12] = [
    &["context", "--json"],
    &["context", "--identity", "--json"],
    &["enter"],
    &["refresh"],
    &["verify"],
    &["repository-point", "--json"],
    &["snapshot", "--json"],
    &["landed", "--json"],
    &["sessions", "--json"],
    &["validate", "--plan"],
    &["run", "--intent", "budget", "--", "true"],
    &["verify"],
];

/// Git processes per command, `binary` on a fresh build of `scenario`:
/// `None` where the command failed (the baseline has no `--identity`).
fn counts(binary: &Path, scenario: &str, base: &Path) -> Vec<Option<usize>> {
    if base.exists() {
        fs::remove_dir_all(base).unwrap();
    }
    let work = base.join("work");
    let home = base.join("home");
    for dir in [&work, &home] {
        fs::create_dir_all(dir).unwrap();
    }
    let cwd = scenarios::build(scenario, &work);
    COUNTED
        .iter()
        .enumerate()
        .map(|(step, args)| {
            let trace = home.join(format!("trace-{step}.json"));
            let output = Command::new(binary)
                .arg("-q")
                .args(*args)
                .current_dir(&cwd)
                .envs(HERMETIC_GIT_ENV)
                .env("HOME", &home)
                .env("XDG_STATE_HOME", home.join("state"))
                .env("TOIMIJA_STATE_DIR", home.join("toimija"))
                .env("GIT_TRACE2_EVENT", &trace)
                .env_remove("TOIMIJA_SESSION")
                .stdin(Stdio::null())
                .output()
                .unwrap();
            // `verify` exits 1 on a stale packet and still counts.
            let counted = output.status.success() || args[0] == "verify";
            counted.then(|| {
                fs::read_to_string(&trace)
                    .unwrap_or_default()
                    .lines()
                    .filter(|line| line.contains(r#""event":"start""#))
                    .count()
            })
        })
        .collect()
}

/// Print the git processes each command starts, the baseline's and this
/// build's, per scenario, as a Markdown table. Asserts nothing; the budgets
/// are pinned in `cli_budgets`.
#[test]
#[ignore = "needs a baseline toimija: TOIMIJA_BASELINE=/path/to/toimija"]
fn git_processes_against_the_baseline() {
    if !git_available() {
        return;
    }
    let baseline = PathBuf::from(
        std::env::var_os("TOIMIJA_BASELINE")
            .expect("set TOIMIJA_BASELINE to the toimija binary to compare against"),
    );
    let candidate = std::env::var_os("TOIMIJA_CANDIDATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_toimija")));
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap().join("s");
    let cell = |count: Option<usize>| count.map_or("–".to_string(), |count| count.to_string());
    let mut table = String::from("| scenario | command | before | after |\n|---|---|---:|---:|\n");
    for scenario in scenarios::ALL {
        let before = counts(&baseline, scenario, &base);
        let after = counts(&candidate, scenario, &base);
        for ((args, before), after) in COUNTED.iter().zip(before).zip(after) {
            table.push_str(&format!(
                "| {scenario} | {} | {} | {} |\n",
                args.join(" "),
                cell(before),
                cell(after)
            ));
        }
    }
    println!("{table}");
}
