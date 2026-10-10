use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::config::{GateSetting, RepoConfig};
use crate::repo::RepoKind;

/// The contract file a repository declares its gate in.
pub const SYKLI_CONTRACT: &str = "sykli.json";

/// The gate exactly as the workflow runs it: run every declared task, keep the
/// receipt, then check the receipt against the contract. `.sykli/` is
/// untracked, so a fresh worktree lacks it and the redirect would fail first. Named, never run —
/// running checks and keeping receipts is Sykli's.
pub const SYKLI_GATE: &str = "mkdir -p .sykli && sykli run sykli.json --json > .sykli/receipt.json && sykli verify .sykli/receipt.json --contract sykli.json";

/// A contract larger than this is not read for its task names; the gate is
/// still named.
const SYKLI_CONTRACT_READ_CAP: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationPlan {
    pub source: ValidationSource,
    /// The gate the repository declares (`sykli.json`), when it declares one.
    /// It replaces the commands Toimija would otherwise infer from the repo's
    /// shape: the declared gate is the fact, the inference a guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<DeclaredGate>,
    /// Commands derived from the changed surface (e.g. `cargo test -p <crate>`
    /// for the crates that actually changed). Advisory and additive — a fast
    /// first pass, never a substitute for the authoritative tiers below. Always
    /// empty when `validation_plan` is built, then enriched once the changed
    /// surface is known (see `context::build_context_packet`), unless the repo
    /// declares a gate.
    pub focused: Vec<String>,
    pub fast: Vec<String>,
    pub strict: Vec<String>,
    pub full: Vec<String>,
    pub privileged: Vec<String>,
    pub notes: Vec<String>,
}

/// A gate the repository declares for itself, read from its contract file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeclaredGate {
    /// The contract file, relative to the repository root.
    pub contract: String,
    /// The exact command that runs the gate and checks its receipt.
    pub command: String,
    /// The task names the contract declares, in file order. Empty when the
    /// contract could not be read; a note then says why.
    pub tasks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationSource {
    RepoConfig,
    Inferred,
    InferredAndRepoConfig,
    /// The repository's declared gate (`sykli.json`).
    Sykli,
    /// The declared gate plus commands from `.toimija/repo.yaml`.
    SykliAndRepoConfig,
    Unknown,
}

pub fn validation_plan(repo_root: &Path, config: &RepoConfig, kind: RepoKind) -> ValidationPlan {
    let (gate, gate_notes) = declared_gate(repo_root, &config.validation.gate);
    let replaces = config.has_validation() && config.validation.replace_inferred;
    let mut plan = if replaces {
        // Replaces what toimija would infer — never the repository's own gate.
        configured_plan(config, true)
    } else if gate.is_some() {
        gate_only_plan()
    } else if config.has_validation() && kind == RepoKind::Generic {
        configured_plan(config, false)
    } else {
        inferred_plan(repo_root, kind)
    };
    if !replaces && config.has_validation() && plan.source != ValidationSource::RepoConfig {
        extend_unique(&mut plan.fast, &config.validation.fast);
        extend_unique(&mut plan.strict, &config.validation.strict);
        extend_unique(&mut plan.full, &config.validation.full);
        extend_unique(&mut plan.privileged, &config.validation.privileged);
        plan.source = ValidationSource::InferredAndRepoConfig;
    }
    if gate.is_some() {
        plan.source = if config.has_validation() {
            ValidationSource::SykliAndRepoConfig
        } else {
            ValidationSource::Sykli
        };
    }
    plan.gate = gate;
    plan.notes.splice(0..0, gate_notes);
    plan
}

fn gate_only_plan() -> ValidationPlan {
    ValidationPlan {
        source: ValidationSource::Sykli,
        gate: None,
        focused: Vec::new(),
        fast: Vec::new(),
        strict: Vec::new(),
        full: Vec::new(),
        privileged: Vec::new(),
        notes: Vec::new(),
    }
}

/// The gate a repository declares in `sykli.json`, with the notes that go
/// with it: that gate, exactly, as `validation.gate` allows. `None` without the
/// file or with `gate: false`. The contract is only read for its task names —
/// no Sykli code, no execution.
fn declared_gate(repo_root: &Path, setting: &GateSetting) -> (Option<DeclaredGate>, Vec<String>) {
    let contract = repo_root.join(SYKLI_CONTRACT);
    let present = contract.is_file();
    let command = match setting {
        GateSetting::Enabled(false) => return (None, Vec::new()),
        GateSetting::Command(_) if !present => {
            return (
                None,
                vec![format!(
                    "validation.gate in .toimija/repo.yaml names a gate command, but there is no \
                     {SYKLI_CONTRACT}; no gate is named."
                )],
            );
        }
        _ if !present => return (None, Vec::new()),
        GateSetting::Enabled(true) => SYKLI_GATE.to_string(),
        GateSetting::Command(command) => command.clone(),
    };
    info!(
        event = "toimija.validation.sykli_gate",
        contract = SYKLI_CONTRACT
    );
    let mut notes = vec![format!(
        "The repository declares its gate in {SYKLI_CONTRACT}; run it as written. It writes \
         .sykli/receipt.json and checks it against the contract."
    )];
    if matches!(setting, GateSetting::Command(_)) {
        notes.push(
            "The gate command is the one validation.gate names in .toimija/repo.yaml.".to_string(),
        );
    }
    let tasks = match sykli_task_names(&contract) {
        Ok(tasks) => {
            if tasks.is_empty() {
                notes.push(format!("{SYKLI_CONTRACT} declares no tasks."));
            }
            tasks
        }
        Err(reason) => {
            notes.push(format!(
                "Task names unavailable: {SYKLI_CONTRACT} could not be read ({reason})."
            ));
            Vec::new()
        }
    };
    (
        Some(DeclaredGate {
            contract: SYKLI_CONTRACT.to_string(),
            command,
            tasks,
        }),
        notes,
    )
}

/// The task names a Sykli contract declares, in file order. Reads only the
/// `tasks[].name` fields; everything else in the contract is Sykli's.
fn sykli_task_names(contract: &Path) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct Contract {
        #[serde(default)]
        tasks: Vec<Task>,
    }
    #[derive(Deserialize)]
    struct Task {
        name: String,
    }
    use std::io::Read as _;
    let mut raw = Vec::new();
    std::fs::File::open(contract)
        .and_then(|file| file.take(SYKLI_CONTRACT_READ_CAP + 1).read_to_end(&mut raw))
        .map_err(|error| error.to_string())?;
    if raw.len() as u64 > SYKLI_CONTRACT_READ_CAP {
        return Err(format!("larger than {SYKLI_CONTRACT_READ_CAP} bytes"));
    }
    let parsed: Contract = serde_json::from_slice(&raw).map_err(|error| error.to_string())?;
    Ok(parsed.tasks.into_iter().map(|task| task.name).collect())
}
fn inferred_plan(repo_root: &Path, kind: RepoKind) -> ValidationPlan {
    if kind == RepoKind::Generic {
        return ValidationPlan {
            source: ValidationSource::Unknown,
            gate: None,
            focused: Vec::new(),
            fast: Vec::new(),
            strict: Vec::new(),
            full: Vec::new(),
            privileged: Vec::new(),
            notes: vec!["validation unavailable for generic repo without repo config".to_string()],
        };
    }

    let Some(pack) = crate::adapters::pack::for_kind(kind) else {
        // A recognized kind without a registry row is a wiring bug; degrade
        // honestly to the generic (unknown) plan.
        return inferred_plan(repo_root, RepoKind::Generic);
    };
    info!(event = "toimija.validation.inferred", repo_kind = pack.name);
    let mut notes = vec![match pack.tier {
        crate::adapters::pack::LanguageTier::Full => format!(
            "Validation inferred from {} repo shape, not proven by Sykli",
            pack.name
        ),
        crate::adapters::pack::LanguageTier::Minimum => format!(
            "Validation inferred from {} repo shape (minimum language pack), not proven by Sykli",
            pack.name
        ),
    }];
    let commands = crate::adapters::pack::proven_commands(repo_root, pack);
    notes.extend(commands.notes);
    ValidationPlan {
        source: ValidationSource::Inferred,
        gate: None,
        focused: Vec::new(),
        full: commands.fast.clone(),
        fast: commands.fast,
        strict: commands.strict,
        privileged: Vec::new(),
        notes,
    }
}

fn configured_plan(config: &RepoConfig, replaces_inferred: bool) -> ValidationPlan {
    ValidationPlan {
        source: ValidationSource::RepoConfig,
        gate: None,
        focused: Vec::new(),
        fast: config.validation.fast.clone(),
        strict: config.validation.strict.clone(),
        full: config.validation.full.clone(),
        privileged: config.validation.privileged.clone(),
        notes: replaces_inferred
            .then(|| "Repository config explicitly replaces inferred validation.".to_string())
            .into_iter()
            .collect(),
    }
}

fn extend_unique(target: &mut Vec<String>, additions: &[String]) {
    for command in additions {
        if !target.contains(command) {
            target.push(command.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_plan_from_config() {
        let mut config = RepoConfig::default();
        config.validation.fast = vec!["just test".to_string()];
        let plan = validation_plan(no_contract().path(), &config, RepoKind::Generic);
        assert_eq!(plan.source, ValidationSource::RepoConfig);
        assert_eq!(plan.fast, vec!["just test"]);
    }

    #[test]
    fn repository_validation_is_additive_and_deduplicated() {
        let mut config = RepoConfig::default();
        config.validation.fast = vec![
            "cargo test --workspace".to_string(),
            "just test-fast".to_string(),
        ];
        config.validation.privileged = vec!["just integration".to_string()];

        let plan = validation_plan(no_contract().path(), &config, RepoKind::Rust);

        assert_eq!(plan.source, ValidationSource::InferredAndRepoConfig);
        assert_eq!(
            serde_json::to_value(&plan).unwrap()["source"],
            "inferred_and_repo_config"
        );
        assert_eq!(plan.fast, vec!["cargo test --workspace", "just test-fast"]);
        assert!(plan.strict.iter().any(|cmd| cmd.contains("cargo clippy")));
        assert_eq!(plan.privileged, vec!["just integration"]);
    }

    #[test]
    fn explicit_replacement_omits_inferred_validation() {
        let mut config = RepoConfig::default();
        config.validation.replace_inferred = true;
        config.validation.fast = vec!["just test".to_string()];

        let plan = validation_plan(no_contract().path(), &config, RepoKind::Rust);

        assert_eq!(plan.source, ValidationSource::RepoConfig);
        assert_eq!(
            serde_json::to_value(&plan).unwrap()["source"],
            "repo_config"
        );
        assert_eq!(plan.fast, vec!["just test"]);
        assert!(plan.strict.is_empty());
        assert!(plan.notes[0].contains("explicitly replaces"));
    }

    #[test]
    fn validation_plan_inferred_for_rust() {
        let plan = validation_plan(no_contract().path(), &RepoConfig::default(), RepoKind::Rust);
        assert_eq!(plan.source, ValidationSource::Inferred);
        assert!(plan.strict.iter().any(|cmd| cmd.contains("cargo clippy")));
    }

    #[test]
    fn validation_plan_inferred_for_elixir() {
        let plan = validation_plan(
            no_contract().path(),
            &RepoConfig::default(),
            RepoKind::Elixir,
        );
        assert_eq!(plan.source, ValidationSource::Inferred);
        assert!(plan.fast.iter().any(|cmd| cmd == "mix test"));
    }

    /// A directory without `sykli.json` (the crate root has one).
    fn no_contract() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn with_contract(contract: &str) -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(SYKLI_CONTRACT), contract).unwrap();
        temp
    }

    #[test]
    fn a_declared_sykli_gate_replaces_the_inferred_plan() {
        let temp = with_contract(
            r#"{"schema":"sykli-contract.v1","tasks":[{"name":"fmt","run":"cargo fmt --check","inputs":[]},{"name":"test","run":"cargo test","inputs":[]}]}"#,
        );

        let plan = validation_plan(temp.path(), &RepoConfig::default(), RepoKind::Rust);

        assert_eq!(plan.source, ValidationSource::Sykli);
        assert_eq!(serde_json::to_value(&plan).unwrap()["source"], "sykli");
        let gate = plan.gate.expect("declared gate");
        assert_eq!(gate.contract, "sykli.json");
        assert_eq!(
            gate.command,
            "mkdir -p .sykli && sykli run sykli.json --json > .sykli/receipt.json && sykli verify .sykli/receipt.json --contract sykli.json"
        );
        assert_eq!(gate.tasks, vec!["fmt", "test"]);
        assert!(plan.fast.is_empty() && plan.strict.is_empty() && plan.full.is_empty());
        assert!(
            plan.notes.iter().all(|note| !note.contains("inferred")),
            "{:?}",
            plan.notes
        );
    }

    #[test]
    fn repo_config_adds_to_a_declared_gate_and_only_gate_false_removes_it() {
        let temp = with_contract(r#"{"tasks":[{"name":"test"}]}"#);
        let mut config = RepoConfig::default();
        config.validation.privileged = vec!["just integration".to_string()];

        let plan = validation_plan(temp.path(), &config, RepoKind::Rust);
        assert_eq!(plan.source, ValidationSource::SykliAndRepoConfig);
        assert!(plan.gate.is_some());
        assert_eq!(plan.privileged, vec!["just integration"]);
        assert!(plan.fast.is_empty());

        // replace_inferred drops guesses, not the declared gate.
        config.validation.replace_inferred = true;
        let replaced = validation_plan(temp.path(), &config, RepoKind::Rust);
        assert_eq!(replaced.source, ValidationSource::SykliAndRepoConfig);
        assert!(replaced.gate.is_some());
        assert!(replaced.strict.is_empty());

        config.validation.gate = crate::config::GateSetting::Enabled(false);
        let opted_out = validation_plan(temp.path(), &config, RepoKind::Rust);
        assert_eq!(opted_out.source, ValidationSource::RepoConfig);
        assert!(opted_out.gate.is_none());
    }

    #[test]
    fn an_unreadable_contract_still_names_the_gate_and_says_why_tasks_are_missing() {
        let temp = with_contract("{ not json");

        let plan = validation_plan(temp.path(), &RepoConfig::default(), RepoKind::Generic);

        assert_eq!(plan.source, ValidationSource::Sykli);
        let gate = plan
            .gate
            .expect("the file declares the gate even when unparseable");
        assert!(gate.tasks.is_empty());
        assert!(
            plan.notes
                .iter()
                .any(|note| note.contains("Task names unavailable")),
            "{:?}",
            plan.notes
        );
    }
}
