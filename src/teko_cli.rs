use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;

use crate::cli::Refusal;
use crate::command::{CommandRunner, ProcessCommandRunner};
use crate::teko::store::{TekoStore, now_secs};
use crate::teko::{
    Capabilities, CompletionLevel, CompletionWitness, GateCommand, GateId, GateReceipt,
    GateRequirement, GateStatus, Goal, GoalId, ManualClaim, Obligation, ObligationId,
    ObligationKind, PathRule, Scope, ScopeStatus, Spec, SpecId, SpecStatus, VerifiedRepository,
    Work, WorkId, WorkStatus, WorkflowStatus, changes_between, evaluate, verified_repository,
};

#[derive(Debug, Args)]
pub struct TekoArgs {
    #[command(subcommand)]
    command: TekoCommand,
}

impl TekoArgs {
    pub fn json(&self) -> bool {
        match &self.command {
            TekoCommand::New(args) => args.json,
            TekoCommand::Start(args) => args.json,
            TekoCommand::Show(args) => args.json,
            TekoCommand::Verify(args) => args.json,
            TekoCommand::Close(args) => args.json,
            TekoCommand::Next(args) => args.json,
        }
    }
}

#[derive(Debug, Subcommand)]
enum TekoCommand {
    /// Create a durable specification; add --ratify to make it activatable.
    New(NewArgs),
    /// Create and activate bounded work from one specification.
    Start(StartArgs),
    /// Show freshly derived active-work status.
    Show(JsonArgs),
    /// Refresh repository truth, claim manual obligations, and run required gates.
    Verify(VerifyArgs),
    /// Close active work only when fresh evaluation has no blockers.
    Close(JsonArgs),
    /// Show the highest-value legal next action without executing it.
    Next(JsonArgs),
}

#[derive(Debug, Args)]
struct NewArgs {
    #[arg(long)]
    id: Option<String>,
    #[arg(long)]
    title: Option<String>,
    #[arg(long)]
    objective: Option<String>,
    #[arg(long = "goal")]
    goals: Vec<String>,
    #[arg(long = "non-goal")]
    non_goals: Vec<String>,
    #[arg(long = "invariant")]
    invariants: Vec<String>,
    /// Operator-approved gate command. Use `id=command` for a stable gate id.
    #[arg(long = "gate")]
    gates: Vec<String>,
    #[arg(long)]
    completion: Option<String>,
    #[arg(long)]
    ratify: bool,
    #[arg(long)]
    source: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct StartArgs {
    #[arg(long)]
    spec: Option<String>,
    #[arg(long)]
    id: Option<String>,
    #[arg(long)]
    objective: Option<String>,
    /// Select goal ids; absence selects every goal.
    #[arg(long = "goal")]
    goals: Vec<String>,
    /// Repository-relative write scope; repeatable.
    #[arg(long = "scope")]
    scope: Vec<PathBuf>,
    /// Explicitly ratify a draft as part of the activation transaction.
    #[arg(long)]
    ratify: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct VerifyArgs {
    /// Explicitly claim a manual obligation by stable id.
    #[arg(long = "claim")]
    claims: Vec<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct JsonArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
struct StatusView<'a> {
    spec: &'a Spec,
    work: &'a Work,
    current: &'a VerifiedRepository,
    status: &'a WorkflowStatus,
}

struct ActiveStatus {
    spec: Spec,
    work: Work,
    current: VerifiedRepository,
    status: WorkflowStatus,
}

pub fn run(args: TekoArgs) -> Result<()> {
    match args.command {
        TekoCommand::New(args) => run_new(args),
        TekoCommand::Start(args) => run_start(args),
        TekoCommand::Show(args) => run_show(args),
        TekoCommand::Verify(args) => run_verify(args),
        TekoCommand::Close(args) => run_close(args),
        TekoCommand::Next(args) => run_next(args),
    }
}

/// Render the ephemeral packet supplied to wrapped workers. It is derived from
/// the same live Toimija state as `teko show`; nothing is persisted.
pub fn active_work_packet(repo_root: &Path, dialect: &str) -> Result<Option<String>> {
    let Some(store) = TekoStore::open_existing(repo_root)? else {
        return Ok(None);
    };
    let Some(active) = load_active_status(repo_root, &store)? else {
        return Ok(None);
    };
    let mut packet = String::new();
    packet.push_str(match dialect {
        "codex" => "ACTIVE TEKO WORK\n\n",
        "claude" => "ACTIVE TEKO WORK\n\n",
        _ => "ACTIVE WORK\n\n",
    });
    packet.push_str(&format!("Objective: {}\n", active.work.objective));
    packet.push_str(&format!(
        "Branch: {}\nCurrent tree: {}\n",
        active.current.point.branch.as_deref().unwrap_or("detached"),
        active.current.point.tree_oid
    ));
    packet.push_str("Changed paths:\n");
    for path in &active.current.changed_paths {
        packet.push_str(&format!("- {}\n", path.display()));
    }
    packet.push_str("Scope: ");
    packet.push_str(match active.status.scope {
        ScopeStatus::WithinScope => "valid\n",
        ScopeStatus::Violated { .. } => "violated\n",
        ScopeStatus::Unknown { .. } => "unknown\n",
    });
    packet.push_str("Remaining obligations:\n");
    for obligation in active
        .status
        .obligations
        .iter()
        .filter(|item| !item.resolved)
    {
        packet.push_str(&format!("- {}\n", obligation.description));
    }
    packet.push_str(&format!(
        "Completion required: {}\n",
        active.status.required_completion
    ));
    if let Some(blocker) = active.status.blockers.first() {
        packet.push_str(&format!("Next: {}\n", blocker.message));
    } else {
        packet.push_str("Next: close the verified work\n");
    }
    Ok(Some(packet))
}

fn run_new(mut args: NewArgs) -> Result<()> {
    let repo_root = repo_root()?;
    let interactive = args.title.is_none() || args.objective.is_none();
    let title = required_or_prompt(args.title.take(), "Title")?;
    let objective = required_or_prompt(args.objective.take(), "Objective")?;
    if args.goals.is_empty() {
        args.goals = prompt_list("Goals (comma-separated)")?;
    }
    if args.non_goals.is_empty() {
        args.non_goals = prompt_list("Non-goals (comma-separated, optional)")?;
    }
    if args.gates.is_empty() {
        args.gates = prompt_list("Required gates (comma-separated, optional)")?;
    }
    let completion = CompletionLevel::from_str(
        args.completion
            .unwrap_or_else(|| {
                if interactive {
                    prompt("Completion level [validated]").unwrap_or_default()
                } else {
                    "validated".to_string()
                }
            })
            .trim(),
    )?;
    if interactive && !args.ratify {
        args.ratify = matches!(
            prompt("Ratify now? [y/N]")?.to_ascii_lowercase().as_str(),
            "y" | "yes"
        );
    }
    let goals = parse_goals(args.goals)?;
    let verification = parse_gates(args.gates)?;
    if completion == CompletionLevel::Validated && verification.is_empty() {
        bail!("validated completion requires at least one operator-approved `--gate`");
    }
    let spec = Spec {
        id: SpecId(args.id.unwrap_or_else(|| mint_id("spec"))),
        title,
        objective,
        goals,
        non_goals: args.non_goals,
        invariants: args.invariants,
        verification,
        completion,
        status: if args.ratify {
            SpecStatus::Ratified
        } else {
            SpecStatus::Draft
        },
        source: args.source,
    };
    TekoStore::open(&repo_root)?.create_spec(&spec)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&spec)?);
    } else {
        println!("created spec {} ({:?})", spec.id, spec.status);
        if spec.status == SpecStatus::Draft {
            println!("start with `--ratify` after reviewing it");
        }
    }
    Ok(())
}

fn run_start(args: StartArgs) -> Result<()> {
    let repo_root = repo_root()?;
    let store = TekoStore::open(&repo_root)?;
    if let Some(active) = store.active_work()? {
        bail!("work `{}` is already active in this repository", active.id);
    }
    let spec = select_spec(&store, args.spec.as_deref(), args.ratify)?;
    let capabilities = Capabilities::for_spec(&spec);
    if !capabilities.supports(spec.completion) {
        bail!(
            "{} completion is unreachable: add an executable gate or ratify a changed-level spec",
            spec.completion
        );
    }
    let origin = refresh_and_verify(&repo_root)?;
    let selected = selected_goals(&spec, &args.goals)?;
    let mut obligations = selected
        .iter()
        .map(|goal| Obligation {
            id: ObligationId(goal.id.0.clone()),
            description: goal.description.clone(),
            kind: ObligationKind::Manual,
        })
        .collect::<Vec<_>>();
    obligations.extend(spec.verification.iter().map(|gate| Obligation {
        id: ObligationId(format!("gate-{}", gate.id)),
        description: format!("{} passes on the current tree", gate.command.shell),
        kind: ObligationKind::Gate {
            gate_id: gate.id.clone(),
        },
    }));
    obligations.extend([
        Obligation {
            id: ObligationId("scope".to_string()),
            description: "changes stay within the declared scope".to_string(),
            kind: ObligationKind::Scope,
        },
        Obligation {
            id: ObligationId("repository-changed".to_string()),
            description: "repository differs from the activation origin".to_string(),
            kind: ObligationKind::RepositoryChanged,
        },
    ]);
    let scope_paths = if args.scope.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.scope
    };
    let scope = Scope {
        write: scope_paths
            .into_iter()
            .map(PathRule::new)
            .collect::<Result<Vec<_>>>()?,
    };
    let work = Work {
        id: WorkId(args.id.unwrap_or_else(|| mint_id("work"))),
        spec_id: spec.id.clone(),
        objective: args.objective.unwrap_or_else(|| spec.objective.clone()),
        scope,
        obligations,
        completion: spec.completion,
        origin: origin.point,
        status: WorkStatus::Open,
    };
    let work = store.activate_work(work, args.ratify)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&work)?);
    } else {
        println!("active work {}", work.id);
        println!("objective: {}", work.objective);
        println!("origin tree: {}", short_oid(&work.origin.tree_oid.0));
    }
    Ok(())
}

fn run_show(args: JsonArgs) -> Result<()> {
    let repo_root = repo_root()?;
    let Some(store) = TekoStore::open_existing(&repo_root)? else {
        return print_no_active(args.json);
    };
    let Some(active) = load_active_status(&repo_root, &store)? else {
        return print_no_active(args.json);
    };
    print_status(&active, args.json)
}

fn run_verify(args: VerifyArgs) -> Result<()> {
    let repo_root = repo_root()?;
    let store = TekoStore::open(&repo_root)?;
    let work = store
        .active_work()?
        .ok_or_else(|| anyhow::anyhow!("No active Teko work to verify."))?;
    for id in args.claims {
        store.put_manual_claim(ManualClaim {
            work_id: work.id.clone(),
            obligation_id: ObligationId(id),
            claimed_at: now_secs(),
        })?;
    }
    let mut active = load_active_status(&repo_root, &store)?
        .ok_or_else(|| anyhow::anyhow!("No active Teko work to verify."))?;
    if matches!(active.status.scope, ScopeStatus::WithinScope) {
        let runner = ProcessCommandRunner;
        for gate in &active.spec.verification {
            let existing = active
                .status
                .gates
                .iter()
                .find(|status| status.id == gate.id)
                .map(|status| &status.status);
            if matches!(existing, Some(GateStatus::Current)) {
                continue;
            }
            let before = refresh_and_verify(&repo_root)?;
            let output = runner
                .run(&repo_root, "sh", &["-c", &gate.command.shell])
                .with_context(|| format!("failed to run gate `{}`", gate.command.shell))?;
            let after = refresh_and_verify(&repo_root)?;
            if before.point.tree_oid != after.point.tree_oid {
                store.append_gate_invalidation(
                    &work,
                    &gate.id.0,
                    "repository tree changed while the gate ran",
                )?;
                continue;
            }
            store.put_gate_receipt(GateReceipt {
                work_id: work.id.clone(),
                gate_id: gate.id.clone(),
                tree_oid: after.point.tree_oid,
                command: gate.command.clone(),
                exit_status: output.status,
                output_digest: GateReceipt::digest(&output.stdout, &output.stderr),
                passed_at: now_secs(),
            })?;
        }
    }
    active = load_active_status(&repo_root, &store)?
        .ok_or_else(|| anyhow::anyhow!("No active Teko work to verify."))?;
    if active.status.closable {
        store.mark_verified(&active.work.id)?;
        active.work.status = WorkStatus::Verified;
    }
    print_status(&active, args.json)
}

fn run_close(args: JsonArgs) -> Result<()> {
    let repo_root = repo_root()?;
    let store = TekoStore::open(&repo_root)?;
    let active = load_active_status(&repo_root, &store)?
        .ok_or_else(|| anyhow::anyhow!("No active Teko work to close."))?;
    if !active.status.closable {
        return Err(cannot_close(&active.status).into());
    }
    let witness = CompletionWitness {
        work_id: active.work.id.clone(),
        repository: active.current.point.clone(),
        closed_at: now_secs(),
    };
    store.close_work(witness.clone())?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&witness)?);
    } else {
        println!(
            "closed work {} on tree {}",
            active.work.id,
            short_oid(&witness.repository.tree_oid.0)
        );
    }
    Ok(())
}

fn run_next(args: JsonArgs) -> Result<()> {
    let repo_root = repo_root()?;
    let store = TekoStore::open(&repo_root)?;
    let active = load_active_status(&repo_root, &store)?
        .ok_or_else(|| anyhow::anyhow!("No active Teko work."))?;
    let next = active.status.next_actions.first();
    if args.json {
        println!("{}", serde_json::to_string_pretty(&next)?);
    } else if let Some(next) = next {
        println!("next: {}", next.action);
        println!("reason: {}", next.reason);
    } else {
        println!("next: close the verified work");
        println!("reason: every required obligation is current");
    }
    Ok(())
}

fn load_active_status(repo_root: &Path, store: &TekoStore) -> Result<Option<ActiveStatus>> {
    let Some(work) = store.active_work()? else {
        return Ok(None);
    };
    let spec = store
        .get_spec(&work.spec_id)?
        .ok_or_else(|| anyhow::anyhow!("active work references missing spec `{}`", work.spec_id))?;
    let current = verified_repository(repo_root)?;
    let scope = changes_between(
        &ProcessCommandRunner,
        repo_root,
        &work.origin.tree_oid,
        &current.point.tree_oid,
    );
    let evidence = store.evidence(&work.id)?;
    let status = evaluate(
        &spec,
        &work,
        &current,
        &evidence,
        Capabilities::for_spec(&spec),
        scope,
    );
    Ok(Some(ActiveStatus {
        spec,
        work,
        current,
        status,
    }))
}

fn refresh_and_verify(repo_root: &Path) -> Result<VerifiedRepository> {
    crate::refresh::Plan {
        for_diff: false,
        budget: None,
        session_dir: None,
    }
    .run(&ProcessCommandRunner, repo_root)
    .context("failed to refresh current Toimija state")?;
    verified_repository(repo_root)
}

fn print_status(active: &ActiveStatus, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&StatusView {
                spec: &active.spec,
                work: &active.work,
                current: &active.current,
                status: &active.status,
            })?
        );
        return Ok(());
    }
    let complete = active
        .status
        .obligations
        .iter()
        .filter(|obligation| obligation.resolved)
        .count();
    println!("{}", active.spec.title.to_uppercase());
    println!();
    println!(
        "active · {complete}/{} complete · current tree {}",
        active.status.obligations.len(),
        if active.current.dirty {
            "dirty"
        } else {
            "clean"
        }
    );
    println!();
    for obligation in &active.status.obligations {
        println!(
            "{} {}",
            if obligation.resolved { "✓" } else { "○" },
            obligation.description
        );
    }
    println!();
    match &active.status.scope {
        ScopeStatus::WithinScope => println!("scope      valid"),
        ScopeStatus::Violated { paths } => println!(
            "scope      violated — {}",
            paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ScopeStatus::Unknown { reason } => println!("scope      unknown — {reason}"),
    }
    for gate in &active.status.gates {
        let status = match &gate.status {
            GateStatus::Current => "passed on current tree".to_string(),
            GateStatus::Stale { tree_oid } => {
                format!("stale — passed on previous tree {}", short_oid(&tree_oid.0))
            }
            GateStatus::Failed { exit_status } => format!("failed — exit {exit_status}"),
            GateStatus::Missing => "not run".to_string(),
        };
        println!("gate       {} — {status}", gate.command.shell);
    }
    println!("completion {} required", active.status.required_completion);
    for blocker in &active.status.blockers {
        println!("blocker    {}", blocker.message);
    }
    println!();
    if let Some(next) = active.status.next_actions.first() {
        println!("next       {}", next.action);
    } else {
        println!("next       close the verified work");
    }
    Ok(())
}

fn print_no_active(json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::json!({"active": null}));
    } else {
        println!("No active Teko. Repository context is available, but no bounded work is active.");
    }
    Ok(())
}

fn cannot_close(status: &WorkflowStatus) -> Refusal {
    let mut message = String::from("Cannot close\n");
    for blocker in &status.blockers {
        message.push_str(&format!("\n- {}", blocker.message));
    }
    Refusal {
        reason: "teko-not-closable",
        message,
    }
}

fn select_spec(store: &TekoStore, requested: Option<&str>, ratify: bool) -> Result<Spec> {
    if let Some(id) = requested {
        return store
            .get_spec(&SpecId(id.to_string()))?
            .ok_or_else(|| anyhow::anyhow!("spec `{id}` does not exist"));
    }
    let mut candidates = store
        .list_specs()?
        .into_iter()
        .filter(|spec| {
            spec.status == SpecStatus::Ratified || (ratify && spec.status == SpecStatus::Draft)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.id.cmp(&right.id));
    match candidates.len() {
        0 => bail!("no activatable spec; create one or pass `start --ratify --spec <ID>`"),
        1 => Ok(candidates.remove(0)),
        _ => bail!("multiple activatable specs; select one with `--spec <ID>`"),
    }
}

fn selected_goals<'a>(spec: &'a Spec, selected: &[String]) -> Result<Vec<&'a Goal>> {
    if selected.is_empty() {
        return Ok(spec.goals.iter().collect());
    }
    let selected: BTreeSet<_> = selected.iter().map(String::as_str).collect();
    let goals: Vec<_> = spec
        .goals
        .iter()
        .filter(|goal| selected.contains(goal.id.0.as_str()))
        .collect();
    if goals.len() != selected.len() {
        bail!(
            "one or more selected goal ids do not exist in spec `{}`",
            spec.id
        );
    }
    Ok(goals)
}

fn parse_goals(values: Vec<String>) -> Result<Vec<Goal>> {
    let mut used = BTreeSet::new();
    values
        .into_iter()
        .map(|value| {
            let (id, description) = split_named(&value);
            let id = unique_id(id, &mut used);
            if description.trim().is_empty() {
                bail!("goal descriptions cannot be empty");
            }
            Ok(Goal {
                id: GoalId(id),
                description,
            })
        })
        .collect()
}

fn parse_gates(values: Vec<String>) -> Result<Vec<GateRequirement>> {
    let mut used = BTreeSet::new();
    values
        .into_iter()
        .map(|value| {
            let (id, shell) = split_named(&value);
            if shell.trim().is_empty() {
                bail!("gate commands cannot be empty");
            }
            Ok(GateRequirement {
                id: GateId(unique_id(id, &mut used)),
                command: GateCommand { shell },
            })
        })
        .collect()
}

fn split_named(value: &str) -> (String, String) {
    if let Some((id, description)) = value.split_once('=')
        && valid_id(id)
        && !description.trim().is_empty()
    {
        return (id.to_string(), description.trim().to_string());
    }
    (slug(value), value.trim().to_string())
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn unique_id(base: String, used: &mut BTreeSet<String>) -> String {
    let base = if base.is_empty() {
        "item".to_string()
    } else {
        base
    };
    let mut candidate = base.clone();
    let mut suffix = 2;
    while !used.insert(candidate.clone()) {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    candidate
}

fn slug(value: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            if separator && !output.is_empty() {
                output.push('-');
            }
            output.push(character);
            separator = false;
        } else {
            separator = true;
        }
    }
    output
}

fn required_or_prompt(value: Option<String>, label: &str) -> Result<String> {
    match value {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => {
            let value = prompt(label)?;
            if value.is_empty() {
                bail!("{label} is required");
            }
            Ok(value)
        }
    }
}

fn prompt_list(label: &str) -> Result<Vec<String>> {
    Ok(prompt(label)?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect())
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}: ");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(value.trim().to_string())
}

fn repo_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    crate::git::discover_repo_root(&ProcessCommandRunner, &cwd).context("not a Git repository")
}

fn mint_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{nanos}-{}", std::process::id())
}

fn short_oid(oid: &str) -> &str {
    oid.get(..oid.len().min(12)).unwrap_or(oid)
}
