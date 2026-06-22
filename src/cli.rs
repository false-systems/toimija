use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use tracing::{info, info_span, warn};

use crate::command::ProcessCommandRunner;
use crate::config::{self, RepoConfig};
use crate::context;
use crate::git;
use crate::logging::LogFormat;
use crate::render;
use crate::repo;
use crate::validate;

#[derive(Debug, Parser)]
#[command(
    name = "toimija",
    version,
    about = "Git-aware local context compiler for coding agents"
)]
pub struct Cli {
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,
    #[arg(short, long, global = true)]
    pub quiet: bool,
    #[arg(long, value_enum, default_value_t = LogFormat::Text, global = true)]
    pub log_format: LogFormat,
    #[arg(long, global = true)]
    pub no_color: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Init,
    Doctor,
    Context(ContextArgs),
    Validate(ValidateArgs),
}

#[derive(Debug, Args)]
pub struct ContextArgs {
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub for_diff: bool,
    #[arg(long)]
    pub budget: Option<usize>,
}

#[derive(Debug, Args)]
pub struct ValidateArgs {
    #[arg(long)]
    pub plan: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Markdown,
    Json,
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}

pub fn run(cli: Cli) -> Result<()> {
    let started = Instant::now();
    let command_name = cli.command.name();
    let output_format = match &cli.command {
        Command::Context(args) if args.json => "json",
        _ => "text",
    };
    let span = info_span!(
        "toimija.command",
        command = command_name,
        output_format,
        config_path = ".toimija/repo.yaml"
    );
    let _guard = span.enter();
    info!(event = "toimija.command.start");

    let result = match cli.command {
        Command::Init => run_init(),
        Command::Doctor => run_doctor(),
        Command::Context(args) => run_context(args),
        Command::Validate(args) => run_validate(args),
    };

    match &result {
        Ok(()) => info!(
            event = "toimija.command.finish",
            duration_ms = started.elapsed().as_millis() as u64
        ),
        Err(error) => warn!(
            event = "toimija.command.finish",
            duration_ms = started.elapsed().as_millis() as u64,
            error = %error
        ),
    }

    result
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::Init => "init",
            Command::Doctor => "doctor",
            Command::Context(_) => "context",
            Command::Validate(_) => "validate",
        }
    }
}

fn run_init() -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).unwrap_or(cwd);
    let report = config::init_repo(&repo_root)?;
    println!("{}", render::render_init_report(&report));
    Ok(())
}

fn run_doctor() -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let report = repo::doctor(&runner, &cwd);
    println!("{}", render::render_doctor_report(&report));
    if report.serious {
        anyhow::bail!("doctor found serious setup failures");
    }
    Ok(())
}

fn run_context(args: ContextArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let packet = context::build_context_packet(&runner, &cwd, args.for_diff)
        .context("failed to build context packet")?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&packet)?);
    } else {
        let budget = args
            .budget
            .or(packet.config_default_budget())
            .unwrap_or(config::DEFAULT_BUDGET);
        println!("{}", render::render_context_markdown(&packet, budget));
    }
    Ok(())
}

fn run_validate(args: ValidateArgs) -> Result<()> {
    if !args.plan {
        anyhow::bail!("v0.0 only supports `toimija validate --plan`");
    }
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;
    let config = RepoConfig::load_optional(&repo_root)?
        .unwrap_or_else(|| RepoConfig::default_for(&repo_root));
    let kind = repo::detect_repo_kind(&repo_root);
    let plan = validate::validation_plan(&repo_root, &config, kind);
    println!("{}", render::render_validation_plan(&plan));
    Ok(())
}
