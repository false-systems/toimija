//! Toimija developer tasks.
//!
//! This binary doubles as the git `pre-commit` hook (via `devx-pre-commit`).
//! When git invokes it, `argv[0]` ends with `pre-commit`, so it runs the hook
//! logic; otherwise it acts as a small dev CLI.
//!
//! Install the hook with:
//!
//! ```text
//! cargo xtask install-pre-commit-hook
//! ```

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Result};
use devx_pre_commit::{install_self_as_hook, locate_project_root, PreCommitContext};
use serde::Deserialize;

mod land;
mod review;

fn run_hook() -> Result<()> {
    let mut ctx = PreCommitContext::from_git_diff(locate_project_root()?)?;
    // Never touch generated code, only hand-written sources.
    ctx.retain_staged_files(|path| {
        path.components()
            .all(|component| component.as_os_str() != OsStr::new("generated"))
    });
    // Format the staged Rust crates and re-stage the results so the commit
    // records the formatted code rather than rejecting it.
    ctx.rustfmt()?;
    ctx.stage_new_changes()?;
    // Lint gate: a commit must not introduce clippy warnings. This mirrors the
    // CI step (`cargo clippy --all-targets -- -D warnings`) so failures surface
    // locally instead of after a push.
    aborting_commit(run_clippy())?;
    // Test gate: the suite must be green before the commit lands. Runs after
    // clippy so the cheaper lint failures surface first.
    aborting_commit(run_tests())?;
    Ok(())
}

/// Hook-path phrasing: the same gate failure aborts a commit rather than a
/// standalone `cargo xtask gate` run.
fn aborting_commit(result: Result<()>) -> Result<()> {
    result.map_err(|error| error.context("commit aborted. Fix the problem and commit again."))
}

/// The full repo gate behind one entrypoint (guardrails R-R.1): the same three
/// checks `sykli.json` declares, in cheapest-first order — plus xtask's own tests,
/// which `default-members = ["."]` hides from plain `cargo test` (xtask is
/// already compiled to run this, so its tests cost almost nothing here). A
/// human, CI, and the harness run this identically as `cargo xtask gate`.
fn run_full_gate() -> Result<()> {
    // `cargo xtask` aliases work from any workspace subdir; unpinned, a
    // `cargo test` run from inside `xtask/` would gate the xtask package
    // instead of toimija. (Not `locate_project_root`: that returns `GIT_DIR`
    // verbatim when set, which would point the hook path into `.git/`.)
    std::env::set_current_dir(repo_toplevel()?)?;
    run_gate(&["fmt", "--check"], "formatting differs from rustfmt")?;
    run_clippy()?;
    run_tests()?;
    run_gate(&["test", "-p", "xtask"], "xtask tests failed")?;
    Ok(())
}

/// The repo root via `git rev-parse --show-toplevel`, with the redirecting git
/// env scrubbed so an inherited `GIT_DIR` can never steer it.
fn repo_toplevel() -> Result<PathBuf> {
    let mut command = Command::new("git");
    command.args(["rev-parse", "--show-toplevel"]);
    for var in REDIRECTING_GIT_ENV {
        command.env_remove(var);
    }
    let output = command.output()?;
    if !output.status.success() {
        bail!("not inside a git repository — run `cargo xtask gate` from the repo");
    }
    Ok(PathBuf::from(
        String::from_utf8(output.stdout)?.trim_end().to_string(),
    ))
}

fn run_clippy() -> Result<()> {
    run_gate(
        &["clippy", "--all-targets", "--", "-D", "warnings"],
        "clippy reported issues",
    )
}

fn run_tests() -> Result<()> {
    run_gate(&["test"], "tests failed")
}

/// Git environment variables that redirect git at a specific repo, index, or
/// worktree. Git exports these when it invokes a hook; if they leak into the
/// gates' `cargo` process, the integration tests' `git` calls and the
/// `toimija`-under-test both act on the real repo instead of their tempdirs —
/// once observed flipping `core.bare = true` and bricking a sibling worktree.
const REDIRECTING_GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_CEILING_DIRECTORIES",
];

/// Run a `cargo` subcommand and fail if it exits non-zero. The git
/// redirection env is scrubbed so the gates run hermetically against tempdirs.
fn run_gate(args: &[&str], what: &str) -> Result<()> {
    let mut command = Command::new(cargo());
    command.args(args);
    for var in REDIRECTING_GIT_ENV {
        command.env_remove(var);
    }
    let status = command.status()?;
    if !status.success() {
        bail!("{what}");
    }
    Ok(())
}

/// Honour the `CARGO` env var git/cargo sets, falling back to `cargo` on PATH.
fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

#[derive(Debug)]
struct Artifact {
    schema: String,
    digest: String,
    producer: String,
    run: Option<String>,
    anchored_at: String,
    verified: bool,
    metadata_covered: bool,
    edges: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    envelope: String,
    schema: String,
    digest: String,
    provenance: Vec<Provenance>,
    producer: Producer,
    anchored_at: String,
}

#[derive(Debug, Deserialize)]
struct Producer {
    tool: String,
    run: Option<String>,
}

#[derive(Debug)]
enum Provenance {
    Git(String),
    Token(String),
    Artifact(String),
    Other(Option<String>),
}

impl<'de> Deserialize<'de> for Provenance {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let kind = value.get("kind").and_then(serde_json::Value::as_str);
        Ok(match kind {
            Some("git") => link_field(&value, "oid").map_or_else(|| Self::Other(None), Self::Git),
            Some("token") => {
                link_field(&value, "token").map_or_else(|| Self::Other(None), Self::Token)
            }
            Some("artifact") => {
                link_field(&value, "digest").map_or_else(|| Self::Other(None), Self::Artifact)
            }
            _ => Self::Other(first_link_field(&value)),
        })
    }
}

impl Provenance {
    fn link(self) -> Option<String> {
        match self {
            Self::Git(value) | Self::Token(value) | Self::Artifact(value) => Some(value),
            Self::Other(value) => value,
        }
    }
}

fn link_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn first_link_field(value: &serde_json::Value) -> Option<String> {
    ["digest", "token", "oid", "id", "ref"]
        .iter()
        .find_map(|field| link_field(value, field))
}

fn run_why(query: &str) -> Result<()> {
    let root = locate_project_root()?;
    print!("{}", render_why(&root, query)?);
    Ok(())
}

fn render_why(root: &Path, query: &str) -> Result<String> {
    let artifacts = scan_artifacts(root)?;
    let roots = chain_roots(&artifacts, query);
    let mut output = String::new();
    let mut seen = HashSet::new();
    for root in roots {
        render_chain(&mut output, &artifacts, root, 0, &mut seen)?;
    }
    for (index, artifact) in artifacts.iter().enumerate() {
        if !artifact.verified && touches(artifact, query) && seen.insert(index) {
            render_artifact(&mut output, artifact, 0)?;
        }
    }
    if seen.is_empty() {
        writeln!(output, "no sealed chain touches {query}")?;
    }
    Ok(output)
}

fn chain_roots(artifacts: &[Artifact], query: &str) -> Vec<usize> {
    let touched: HashSet<usize> = artifacts
        .iter()
        .enumerate()
        .filter_map(|(index, artifact)| {
            (artifact.verified && touches(artifact, query)).then_some(index)
        })
        .collect();
    let mut roots: Vec<usize> = touched
        .iter()
        .copied()
        .filter(|index| {
            !touched
                .iter()
                .any(|parent| parent != index && links_to(&artifacts[*index], &artifacts[*parent]))
        })
        .collect();
    roots.sort_by_key(|index| {
        (
            artifacts[*index].schema != toimija::context::CONTEXT_SCHEMA,
            *index,
        )
    });
    roots
}

fn render_chain(
    output: &mut String,
    artifacts: &[Artifact],
    index: usize,
    depth: usize,
    seen: &mut HashSet<usize>,
) -> Result<()> {
    if !seen.insert(index) {
        return Ok(());
    }
    render_artifact(output, &artifacts[index], depth)?;
    let mut children: Vec<usize> = artifacts
        .iter()
        .enumerate()
        .filter_map(|(child, artifact)| {
            (artifact.verified && child != index && links_to(artifact, &artifacts[index]))
                .then_some(child)
        })
        .collect();
    children.sort_unstable();
    for child in children {
        render_chain(output, artifacts, child, depth + 1, seen)?;
    }
    Ok(())
}

fn links_to(child: &Artifact, parent: &Artifact) -> bool {
    if child.metadata_covered
        && parent.metadata_covered
        && child.anchored_at == parent.anchored_at
        && !child.anchored_at.is_empty()
        && parent.schema == toimija::context::CONTEXT_SCHEMA
        && child.schema != toimija::context::CONTEXT_SCHEMA
    {
        return true;
    }
    child.metadata_covered
        && child.edges.iter().any(|edge| {
            edge == &parent.digest
                || (parent.schema == toimija::context::CONTEXT_SCHEMA
                    && edge == &parent.anchored_at)
        })
}

fn render_artifact(output: &mut String, artifact: &Artifact, depth: usize) -> Result<()> {
    for _ in 0..depth {
        output.push_str("  ");
    }
    if depth > 0 {
        output.push_str("← ");
    }
    write!(
        output,
        "{} {} ({}, ",
        artifact.schema,
        short(&artifact.digest),
        artifact.producer
    )?;
    if depth > 0 {
        write!(output, "anchored {}", short(&artifact.anchored_at))?;
    } else {
        write!(output, "{}", short(&artifact.anchored_at))?;
    }
    if !artifact.verified {
        output.push_str(") [digest mismatch]\n");
    } else {
        output.push_str(")\n");
    }
    Ok(())
}

fn touches(artifact: &Artifact, query: &str) -> bool {
    artifact.digest == query
        || artifact.anchored_at == query
        || artifact.producer == query
        || artifact.run.as_deref() == Some(query)
        || artifact.edges.iter().any(|edge| edge == query)
}

fn scan_artifacts(root: &Path) -> Result<Vec<Artifact>> {
    let mut files = Vec::new();
    push_if_exists(&mut files, root.join(".toimija/current.json"));
    let sessions = root.join(".toimija/sessions");
    if let Ok(entries) = fs::read_dir(&sessions) {
        for entry in entries.flatten() {
            let dir = entry.path();
            push_if_exists(&mut files, dir.join("current.json"));
        }
    }
    let runs = root.join(".toimija/runs");
    if let Ok(entries) = fs::read_dir(&runs) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("jsonl")) {
                files.push(path);
            }
        }
    }
    let mut artifacts = Vec::new();
    for path in files {
        let raw = fs::read_to_string(&path)?;
        if path.extension() == Some(OsStr::new("jsonl")) {
            for line in raw.lines() {
                collect_artifact(line, &mut artifacts)?;
            }
        } else {
            collect_artifact(&raw, &mut artifacts)?;
        }
    }
    Ok(artifacts)
}

fn push_if_exists(files: &mut Vec<PathBuf>, path: PathBuf) {
    if path.exists() {
        files.push(path);
    }
}

fn collect_artifact(raw: &str, artifacts: &mut Vec<Artifact>) -> Result<()> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(());
    };
    // Toimija's packets carry their envelope under `seal`; other tools'
    // artifacts (kisko's model calls, say) carry the same envelope under
    // `sauma`, where each key has its own digest rule.
    let (envelope, actual) = if let Some(seal) = value.get(toimija::seal::KEY).cloned() {
        let envelope: Envelope = serde_json::from_value(seal)?;
        if envelope.envelope != toimija::seal::ENVELOPE_SCHEMA {
            bail!("unsupported seal envelope `{}`", envelope.envelope);
        }
        (envelope, toimija::seal::digest_of(&value))
    } else if let Some(sauma) = value.get(toimija::seal::SAUMA_KEY).cloned() {
        let envelope: Envelope = serde_json::from_value(sauma)?;
        if !toimija::seal::is_supported_envelope(&envelope.envelope) {
            bail!("unsupported envelope `{}`", envelope.envelope);
        }
        (envelope, toimija::seal::digest_under_sauma_key(&value))
    } else {
        return Ok(());
    };
    if artifacts
        .iter()
        .any(|artifact| artifact.digest == envelope.digest)
    {
        return Ok(());
    }
    let metadata_covered = envelope.envelope == toimija::seal::ENVELOPE_SCHEMA;
    let edges: Vec<String> = if metadata_covered {
        envelope
            .provenance
            .into_iter()
            .filter_map(Provenance::link)
            .collect()
    } else {
        Vec::new()
    };
    let run = envelope.producer.run;
    let producer = run
        .as_ref()
        .map(|run| format!("{}/{}", envelope.producer.tool, run))
        .unwrap_or(envelope.producer.tool);
    artifacts.push(Artifact {
        schema: envelope.schema,
        digest: envelope.digest.clone(),
        producer,
        run,
        anchored_at: envelope.anchored_at,
        verified: actual == envelope.digest,
        metadata_covered,
        edges,
    });
    Ok(())
}

fn short(value: &str) -> String {
    value
        .strip_prefix("sha256:")
        .unwrap_or(value)
        .chars()
        .take(12)
        .collect()
}

fn main() -> Result<()> {
    // Git runs the hook as `.git/hooks/pre-commit`, so argv[0] contains it.
    if std::env::args()
        .next()
        .is_some_and(|arg0| arg0.contains("pre-commit"))
    {
        return run_hook();
    }
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("install-pre-commit-hook") => {
            install_self_as_hook(&locate_project_root()?)?;
            eprintln!("Installed git pre-commit hook (.git/hooks/pre-commit).");
            eprintln!(
                "It runs rustfmt on staged crates, re-stages, then clippy -D warnings and tests."
            );
        }
        Some("why") => {
            let Some(query) = args.get(2) else {
                bail!("usage: cargo xtask why <digest|token|session-id>");
            };
            run_why(query)?;
        }
        Some("gate") => run_full_gate()?,
        Some("land") => {
            // 0 landed, 1 refused, 2 could not tell.
            let code = land::main(&locate_project_root()?, &args[2..]);
            std::process::exit(i32::from(code));
        }
        Some("review") => {
            // The exit code is the verdict: 0 may cross, 1 blocking, 2 no review.
            let code = review::main(&locate_project_root()?, &args[2..]);
            std::process::exit(i32::from(code));
        }
        other => {
            eprintln!("toimija dev tasks:");
            eprintln!("  cargo xtask gate                      run the full gate: fmt --check, clippy -D warnings, tests");
            eprintln!("  cargo xtask install-pre-commit-hook   install the git pre-commit hook");
            eprintln!(
                "  cargo xtask why <query>              walk the local chain of sealed artifacts"
            );
            eprintln!("  cargo xtask review                    a fresh agent reviews this branch: fine, look or blocking");
            eprintln!("  cargo xtask land                      put this branch on main when it is proven and reviewed");
            // Fail closed: a caller that treats the exit code as a verdict (the
            // harness gate, CI) must never mistake "task not found" for a pass.
            if let Some(arg) = other {
                bail!("unknown task: {arg}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};
    use toimija::command::ProcessCommandRunner;

    #[test]
    fn renders_cross_tool_chain_and_reports_tampering() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = temp_repo("seal-crosstool");
        init_git(&root);
        write_context_packet(&root);
        let packet: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(root.join(".toimija/current.json")).unwrap())
                .unwrap();
        let token = packet["consistency"]["token"].as_str().unwrap();
        let model_call = kisko_event(token, false);
        assert_eq!(
            model_call["sauma"]["digest"].as_str().unwrap(),
            toimija::seal::digest_under_sauma_key(&model_call)
        );
        fs::create_dir_all(root.join(".toimija/runs")).unwrap();
        fs::write(
            root.join(".toimija/runs/x.jsonl"),
            format!("{}\n", serde_json::to_string(&model_call).unwrap()),
        )
        .unwrap();

        let output = render_why(&root, token).unwrap();
        assert!(output.contains("toimija-context.v3"));
        assert!(output.contains("  ← kisko-model-call.v1"));
        assert!(!output.contains("digest mismatch"));

        let tampered = kisko_event(token, true);
        fs::write(
            root.join(".toimija/runs/x.jsonl"),
            format!("{}\n", serde_json::to_string(&tampered).unwrap()),
        )
        .unwrap();
        let output = render_why(&root, token).unwrap();
        assert!(output.contains("toimija-context.v3"));
        assert!(output.contains("kisko-model-call.v1"));
        assert!(output.contains("digest mismatch"));
        assert!(!output.contains("  ← kisko-model-call.v1"));
    }

    #[test]
    fn walker_rejects_unknown_envelope_versions() {
        let mut artifact = kisko_event(&format!("sha256:{}", "a".repeat(64)), false);
        artifact["sauma"]["envelope"] = json!("sauma-envelope.v999");

        let error = collect_artifact(&artifact.to_string(), &mut Vec::new()).unwrap_err();
        assert!(error.to_string().contains("v999"));

        // A seal is v2 only: the legacy form was never written under `seal`.
        let mut sealed = kisko_event(&format!("sha256:{}", "a".repeat(64)), false);
        let mut envelope = sealed.as_object_mut().unwrap().remove("sauma").unwrap();
        envelope["envelope"] = json!(toimija::seal::LEGACY_ENVELOPE_SCHEMA);
        sealed["seal"] = envelope;
        let error = collect_artifact(&sealed.to_string(), &mut Vec::new()).unwrap_err();
        assert!(error.to_string().contains("sauma-envelope.v1"), "{error}");
    }

    fn temp_repo(name: &str) -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{name}-{id}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn init_git(root: &Path) {
        git(root, &["init"]);
        git(root, &["config", "user.email", "toimija@example.invalid"]);
        git(root, &["config", "user.name", "Toimija Test"]);
        fs::write(root.join("README.md"), "# Demo\n").unwrap();
        git(root, &["add", "README.md"]);
        git(root, &["commit", "-m", "initial"]);
    }

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn write_context_packet(root: &Path) {
        let runner = ProcessCommandRunner;
        let mut packet = toimija::context::build_context_packet(&runner, root, true).unwrap();
        packet.attach_seal(None).unwrap();
        fs::create_dir_all(root.join(".toimija")).unwrap();
        fs::write(
            root.join(".toimija/current.json"),
            serde_json::to_string_pretty(&packet).unwrap(),
        )
        .unwrap();
    }

    fn kisko_event(token: &str, tamper_digest: bool) -> serde_json::Value {
        let mut event = json!({
            "schema": "kisko-model-call.v1",
            "payload": {
                "model": "test-model",
                "request": "review"
            },
            "sauma": null
        });
        event["sauma"] = json!({
            "envelope": "sauma-envelope.v2",
            "schema": "kisko-model-call.v1",
            "digest": "",
            "provenance": [
                {
                    "kind": "context_fragment",
                    "id": "fragment-1"
                },
                {
                    "kind": "contract",
                    "ref": "contract-1"
                }
            ],
            "producer": {
                "tool": "kisko",
                "version": "0.0.0",
                "run": "call-1"
            },
            "anchored_at": token
        });
        let digest = toimija::seal::digest_under_sauma_key(&event);
        event["sauma"]["digest"] = json!(if tamper_digest { "sha256:bad" } else { &digest });
        event
    }
}
