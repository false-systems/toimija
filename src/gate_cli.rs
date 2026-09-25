use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::{GateInspectArgs, GateRunArgs, GateStatusArgs, GatesArgs, GatesCommand, JsonArgs};
use crate::command::ProcessCommandRunner;
use crate::config::RepoConfig;
use crate::gates::{
    CatalogEntry, GateCatalog, GateId, GateOutcome, GateReceipt, GateSubject, PreparedGate,
    ReceiptApplicability, ResolvedGate, persist_receipt, read_receipts, receipt_applicability,
    verify_subject,
};

#[derive(Serialize)]
struct CatalogRow {
    id: String,
    kind: &'static str,
    description: Option<String>,
    gates: Vec<String>,
    spec_digest: Option<String>,
}

#[derive(Serialize)]
struct GateRunRecord {
    gate: String,
    outcome: &'static str,
    duration_ms: Option<u64>,
    receipt: Option<GateReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct GateRunReport {
    subject: GateSubject,
    current_subject: Option<GateSubject>,
    current: bool,
    gates: Vec<GateRunRecord>,
}

#[derive(Serialize)]
struct GateStatusRow {
    gate: String,
    status: String,
    receipt: Option<String>,
    receipt_subject: Option<GateSubject>,
}

pub fn run(args: GatesArgs) -> Result<i32> {
    match args.command {
        GatesCommand::List(args) => list(args),
        GatesCommand::Inspect(args) => inspect(args),
        GatesCommand::Run(args) => run_gate(args),
        GatesCommand::Status(args) => status(args),
    }
}

fn list(args: JsonArgs) -> Result<i32> {
    let (_, catalog) = load_catalog()?;
    let mut rows = catalog
        .gates()
        .map(|gate| CatalogRow {
            id: gate.spec().id.to_string(),
            kind: "gate",
            description: Some(gate.spec().description.clone()),
            gates: Vec::new(),
            spec_digest: Some(gate.digest().0.clone()),
        })
        .collect::<Vec<_>>();
    rows.extend(catalog.groups().map(|(id, gates)| CatalogRow {
        id: id.to_string(),
        kind: "group",
        description: None,
        gates: gates.iter().map(ToString::to_string).collect(),
        spec_digest: None,
    }));
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        println!("GATES\n");
        for row in rows {
            if row.kind == "group" {
                println!("{:<20} group: {}", row.id, row.gates.join(", "));
            } else {
                println!(
                    "{:<20} {}",
                    row.id,
                    row.description.as_deref().unwrap_or_default()
                );
            }
        }
    }
    Ok(0)
}

fn inspect(args: GateInspectArgs) -> Result<i32> {
    let (_, catalog) = load_catalog()?;
    let id = GateId::parse(args.id)?;
    let entry = catalog
        .inspect(&id)
        .with_context(|| format!("unknown gate or group `{id}`"))?;
    match entry {
        CatalogEntry::Group(gates) => {
            let view = serde_json::json!({
                "id": id,
                "kind": "group",
                "gates": gates,
            });
            if args.json {
                println!("{}", serde_json::to_string_pretty(&view)?);
            } else {
                println!("GATE {id}\n\nkind\n  group\n\ngates");
                for gate in gates {
                    println!("  {gate}");
                }
            }
        }
        CatalogEntry::Gate(gate) => {
            let spec = gate.spec();
            let arguments = spec
                .args
                .iter()
                .map(|arg| arg.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let cwd = match &spec.cwd {
                crate::gates::GateWorkingDirectory::RepositoryRoot => ".".to_string(),
                crate::gates::GateWorkingDirectory::Relative(path) => path.display().to_string(),
            };
            let view = serde_json::json!({
                "id": spec.id,
                "description": spec.description,
                "program": spec.program.to_string(),
                "arguments": arguments,
                "working_directory": cwd,
                "timeout_seconds": spec.timeout.as_secs(),
                "output_limit_bytes": spec.output_limit,
                "success_codes": spec.success_codes,
                "source_mutation": spec.source_mutation,
                "network": spec.network,
                "evidence_grade": spec.evidence_grade,
                "spec_digest": gate.digest(),
            });
            if args.json {
                println!("{}", serde_json::to_string_pretty(&view)?);
            } else {
                println!("GATE {id}");
                println!("\nprogram\n  {}", spec.program);
                println!("\narguments\n  {}", arguments.join(" "));
                println!("\nworking directory\n  {cwd}");
                println!("\ntimeout\n  {}s", spec.timeout.as_secs());
                println!("\nsource mutation\n  forbidden");
                println!(
                    "\nnetwork\n  {}",
                    match spec.network {
                        crate::gates::NetworkPolicy::Deny => "denied",
                        crate::gates::NetworkPolicy::Allow => "allowed",
                        crate::gates::NetworkPolicy::UnenforcedDenyRequested =>
                            "denial requested, not enforced",
                    }
                );
                println!("\nspec digest\n  {}", gate.digest().0);
            }
        }
    }
    Ok(0)
}

fn run_gate(args: GateRunArgs) -> Result<i32> {
    let (repo_root, catalog) = load_catalog()?;
    let id = GateId::parse(args.id)?;
    let resolved = catalog.resolve(&id)?;
    let verified = verify_subject(&repo_root)?;
    let report = execute(&repo_root, verified.subject().clone(), &verified, resolved)?;
    let passed = report.current && report.gates.iter().all(|record| record.outcome == "passed");
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("RUN GATE {id}");
        println!(
            "\nsubject\n  repository point {}\n  tree {}",
            report.subject.repository_point, report.subject.tree_oid.0
        );
        println!("\nexecution\n  isolated snapshot");
        println!("\nresult");
        render_run_records(&report.gates);
        if !report.current {
            println!("\n  receipts are historical; the current repository point changed");
        }
    }
    Ok(if passed { 0 } else { 1 })
}

fn status(args: GateStatusArgs) -> Result<i32> {
    let (repo_root, catalog) = load_catalog()?;
    let subject = verify_subject(&repo_root)?;
    let receipts = read_receipts(&repo_root)?;
    let mut required = args
        .gate
        .into_iter()
        .map(GateId::parse)
        .collect::<Result<Vec<_>>>()?;
    if required.is_empty() {
        required.extend(catalog.gates().map(|gate| gate.spec().id.clone()));
    }
    let resolved = resolve_required(&catalog, &required)?;
    let mut rows = Vec::new();
    for gate in resolved {
        let matching = receipts
            .iter()
            .rev()
            .filter(|receipt| receipt.gate() == &gate.spec().id)
            .collect::<Vec<_>>();
        let current = matching.iter().copied().find(|receipt| {
            receipt_applicability(receipt, subject.subject(), &gate)
                == ReceiptApplicability::Current
        });
        let receipt = current.or_else(|| matching.first().copied());
        let applicability =
            receipt.map(|receipt| receipt_applicability(receipt, subject.subject(), &gate));
        rows.push(GateStatusRow {
            gate: gate.spec().id.to_string(),
            status: applicability
                .map(applicability_name)
                .unwrap_or("missing")
                .to_string(),
            receipt: receipt.map(|receipt| receipt.id().to_string()),
            receipt_subject: receipt.map(|receipt| receipt.subject().clone()),
        });
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        println!("GATE STATUS\n");
        for row in &rows {
            println!(
                "{:<20} {}{}",
                row.gate,
                row.status,
                row.receipt
                    .as_ref()
                    .map(|receipt| format!("  {receipt}"))
                    .unwrap_or_default()
            );
        }
    }
    Ok(if rows.iter().all(|row| row.status == "current") {
        0
    } else {
        1
    })
}

fn execute(
    repo_root: &Path,
    subject: GateSubject,
    verified: &crate::gates::VerifiedGateSubject,
    gates: Vec<ResolvedGate>,
) -> Result<GateRunReport> {
    let mut records = Vec::new();
    for gate in gates {
        let outcome = PreparedGate::prepare(gate, verified)?.execute();
        let record = match outcome {
            GateOutcome::Passed(passed) => {
                let receipt = passed.into_receipt()?;
                persist_receipt(repo_root, &receipt)?;
                GateRunRecord {
                    gate: receipt.gate().to_string(),
                    outcome: "passed",
                    duration_ms: Some(receipt.duration_ms()),
                    details: None,
                    receipt: Some(receipt),
                }
            }
            GateOutcome::Failed(failed) => failure_record(
                failed.gate.to_string(),
                "failed",
                Some(failed.execution.duration_ms),
                serde_json::to_value(failed)?,
            ),
            GateOutcome::TimedOut(timeout) => failure_record(
                timeout.gate.to_string(),
                "timed_out",
                Some(timeout.execution.duration_ms),
                serde_json::to_value(timeout)?,
            ),
            GateOutcome::SpawnFailed(failure) => failure_record(
                failure.gate.to_string(),
                "spawn_failed",
                None,
                serde_json::to_value(failure)?,
            ),
            GateOutcome::SubjectMutated(mutation) => failure_record(
                mutation.gate.to_string(),
                "subject_mutated",
                Some(mutation.execution.duration_ms),
                serde_json::to_value(mutation)?,
            ),
            GateOutcome::EvidenceIncomplete(failure) => failure_record(
                failure.gate.to_string(),
                "evidence_incomplete",
                Some(failure.execution.duration_ms),
                serde_json::to_value(failure)?,
            ),
            GateOutcome::Cancelled(cancellation) => failure_record(
                cancellation.gate.to_string(),
                "cancelled",
                Some(cancellation.execution.duration_ms),
                serde_json::to_value(cancellation)?,
            ),
        };
        records.push(record);
    }
    let current_subject = verify_subject(repo_root).ok();
    let current = current_subject
        .as_ref()
        .is_some_and(|current| current.subject() == &subject);
    Ok(GateRunReport {
        subject,
        current_subject: current_subject.map(|subject| subject.subject().clone()),
        current,
        gates: records,
    })
}

fn failure_record(
    gate: String,
    outcome: &'static str,
    duration_ms: Option<u64>,
    details: serde_json::Value,
) -> GateRunRecord {
    GateRunRecord {
        gate,
        outcome,
        duration_ms,
        receipt: None,
        details: Some(details),
    }
}

fn render_run_records(records: &[GateRunRecord]) {
    for record in records {
        let mark = if record.outcome == "passed" {
            "✓"
        } else {
            "✗"
        };
        let duration = record
            .duration_ms
            .map(|duration| format!("  {:.1}s", duration as f64 / 1000.0))
            .unwrap_or_default();
        println!("  {mark} {:<18} {}{duration}", record.gate, record.outcome);
        if let Some(receipt) = &record.receipt {
            println!("    receipt {}", receipt.id());
        }
    }
}

fn load_catalog() -> Result<(PathBuf, GateCatalog)> {
    let repo_root = repo_root()?;
    let config = RepoConfig::load_optional(&repo_root)?
        .unwrap_or_else(|| RepoConfig::default_for(&repo_root));
    let catalog = GateCatalog::load(&repo_root, &config)?;
    Ok((repo_root, catalog))
}

fn repo_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    crate::git::discover_repo_root(&ProcessCommandRunner, &cwd).context("not a Git repository")
}

fn resolve_required(catalog: &GateCatalog, ids: &[GateId]) -> Result<Vec<ResolvedGate>> {
    let mut seen = BTreeSet::new();
    let mut resolved = Vec::new();
    for id in ids {
        for gate in catalog.resolve(id)? {
            if seen.insert(gate.spec().id.clone()) {
                resolved.push(gate);
            }
        }
    }
    Ok(resolved)
}

fn applicability_name(value: ReceiptApplicability) -> &'static str {
    match value {
        ReceiptApplicability::Current => "current",
        ReceiptApplicability::StaleRepositoryPoint => "stale repository point",
        ReceiptApplicability::StaleTree => "stale tree",
        ReceiptApplicability::GateDefinitionChanged => "gate definition changed",
        ReceiptApplicability::RepositoryMismatch => "repository mismatch",
        ReceiptApplicability::InvalidReceipt => "missing or invalid",
    }
}
