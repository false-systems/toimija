use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use tracing::{info, info_span, warn};

use crate::adoption;
use crate::audit_plan;
use crate::command::ProcessCommandRunner;
use crate::config::{self, RepoConfig};
use crate::consistency::{CONSISTENCY_ALGORITHM, Consistency, GitOperationState, Verdict};
use crate::context;
use crate::footprint::{self, HistoryEnd};
use crate::git;
use crate::handoff;
use crate::landed;
use crate::logging::LogFormat;
use crate::refresh;
use crate::render;
use crate::repo;
use crate::session;
use crate::sessions;
use crate::snapshot;
use crate::snippet;
use crate::validate;
use crate::watch;

#[derive(Debug, Parser)]
#[command(
    name = "toimija",
    version,
    about = "Git-aware local context compiler for coding agents",
    long_about = "Git-aware local context compiler for coding agents.\n\n\
        Toimija starts from Git's state machine — branch, HEAD, index, merge-base, \
        staged/unstaged/untracked files — and compiles it into a compact work packet \
        an agent reads before touching the repo, instead of crawling it cold.",
    after_help = "Typical agent workflow:\n  \
        toimija enter --for-diff --budget 4000    # preflight: refresh + print the work packet\n  \
        toimija run --intent \"fix #51\" -- codex   # wrap an agent in a live session packet\n  \
        toimija sessions                          # who else is working in this repo right now\n  \
        toimija validate --plan                   # print (never run) the validation plan\n\n\
        Boundaries: Toimija is advisory and local-only. It reads Git state, writes only \
        under .toimija/, executes no validation, starts no daemons, and never touches \
        the network."
)]
pub struct Cli {
    /// Increase log verbosity: -v for info, -vv for debug. Logs go to stderr.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,
    /// Suppress logs. Command output still goes to stdout.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Log format written to stderr.
    #[arg(long, value_enum, default_value_t = LogFormat::Text, global = true)]
    pub log_format: LogFormat,
    /// Disable ANSI colour in logs.
    #[arg(long, global = true)]
    pub no_color: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create .toimija/repo.yaml. Agent docs (AGENTS.md/CLAUDE.md) stay yours;
    /// run `toimija snippet preflight` to seed them.
    ///
    /// Use this once per repo to seed the config. It never overwrites an
    /// existing file. Writes only .toimija/repo.yaml.
    Init,
    /// Install the canonical Toimija preamble in the repo's agent entry files.
    ///
    /// Use this once when adopting Toimija into an existing repository. With
    /// `--hooks`, also install best-effort Git freshness hooks.
    #[command(
        after_help = "Examples:\n  toimija adopt             # adopt existing entry files\n  \
        toimija adopt --hooks  # adopt and install refresh hooks"
    )]
    Adopt(AdoptArgs),
    /// Check Toimija setup: git, config, repo kind, and agent docs.
    ///
    /// Use this when setting up a repo or gating CI: it verifies git is
    /// available, the config parses, the repo kind is detected, and the
    /// boundaries in .toimija/repo.yaml have not drifted from the agent docs.
    /// Read-only; advisory by default, `--strict` escalates warnings to
    /// failures.
    #[command(
        after_help = "Examples:\n  toimija doctor            # advisory setup check\n  \
        toimija doctor --strict   # CI / pre-commit gate: drift fails the build"
    )]
    Doctor(DoctorArgs),
    /// Compile the current Git work state into a context packet.
    ///
    /// Use this to *see* the packet without writing anything: branch, HEAD,
    /// merge-base, changed surface, relevance hints (scope, read order,
    /// coupling, risk), and the validation plan. `enter` prints the same packet
    /// and also writes `.toimija/current.*` — prefer it as the agent preflight.
    /// Read-only; no validation execution, no network.
    #[command(
        after_help = "Examples:\n  toimija context --for-diff --budget 4000   # diff-focused, capped for a prompt\n  \
        toimija context --json                     # machine-readable packet"
    )]
    Context(ContextArgs),
    /// Joskus-facing current packet contract: write or verify `.toimija/current.*`.
    ///
    /// Use this from a harness: `--write` synchronously (re)writes the packet
    /// files, `--verify` checks the packet's consistency token against the live
    /// repo and exits non-zero when stale — the verify-before-read gate a
    /// harness calls before trusting the packet.
    #[command(
        after_help = "Examples:\n  toimija current --write            # (re)write .toimija/current.md + current.json\n  \
        toimija current --verify --json    # harness verifier: exit 0 fresh, 1 stale"
    )]
    Current(CurrentArgs),
    /// Recompute the live packet and write .toimija/current.md + current.json.
    ///
    /// Use this when you want the packet files refreshed without printing them
    /// (e.g. from a hook or script); `enter` is refresh + print. Read-only
    /// except for those two files. Runs no validation, no network.
    Refresh(ContextArgs),
    /// Agent entry: write the live packet, then print it as the work preflight.
    ///
    /// The recommended command an agent runs on entering the repo: it refreshes
    /// `.toimija/current.*` and prints the same compact packet as `context`.
    /// Use this before reading files broadly — the packet's Git state and
    /// relevance hints tell you what to read first. Writes only the two packet
    /// files; no validation execution, no network.
    #[command(
        after_help = "Examples:\n  toimija enter --for-diff --budget 4000   # the standard agent preflight\n  \
        toimija enter --json                     # machine-readable entry packet"
    )]
    Enter(ContextArgs),
    /// Watch the repo and refresh .toimija/current.* on change (foreground).
    ///
    /// Use this for a human-driven session where an editor open in the repo
    /// should keep the packet current. Debounced; ignores build dirs and its
    /// own writes. No daemon, no network, no validation execution — runs only
    /// in the foreground until stopped (Ctrl-C). `run` bundles this same watch
    /// around a wrapped agent command.
    Watch(ContextArgs),
    /// Enter, watch while an agent command runs, then exit with its status.
    ///
    /// The flagship wrapper — use this to wrap any agent command in a live
    /// session: it opens a per-session packet directory under
    /// `.toimija/sessions/<id>/`, prints the entry packet, keeps it fresh while
    /// the command runs, mirrors `.toimija/current.*`, and exits with the
    /// wrapped command's status (128+signal preserved). `--intent` is required
    /// (proposal 0006: declaring why the session exists is the entry fee;
    /// `--intent unknown` is the explicit escape hatch). Known agents (claude,
    /// codex, gemini) get the packet inlined in their launch prompt — pass the task via
    /// `--task`; other commands keep the TOIMIJA_* env handshake behind an
    /// honest banner. `--scope` tells sibling sessions what you expect to touch
    /// — advisory, never an assignment.
    #[command(
        after_help = "Examples:\n  toimija run --intent \"fix #51\" --task \"fix the login redirect\" -- claude   # packet + task in the launch prompt\n  \
        toimija run --intent \"fix #51\" --scope src/cli.rs -- codex   # declare intent/scope for siblings\n  \
        toimija run --intent unknown --for-diff --budget 4000 -- sh -c '…'   # tune the packet the child sees"
    )]
    Run(RunArgs),
    /// Declare or update this session's intent — tell siblings why you are here.
    ///
    /// Use this from inside a `toimija run` session (the wrapped agent itself,
    /// mid-work): the session id comes from the TOIMIJA_SESSION env handshake.
    /// Sets the marker's intent and appends any `--scope` paths, so `toimija
    /// sessions` shows the others why this session exists. Advisory — never an
    /// assignment, never a lock. Writes one marker file; no network.
    #[command(
        after_help = "Examples:\n  toimija intent \"migrating the auth module\"\n  \
        toimija intent \"fix #51\" --scope src/cli.rs --scope tests/cli_run.rs"
    )]
    Intent(IntentArgs),
    /// Append or inspect a session handoff record.
    ///
    /// Use this from inside a `toimija run` session when pausing or finishing:
    /// unlike `intent` (why I am here), a handoff says what I leave behind.
    /// Writing uses the TOIMIJA_SESSION env handshake and appends one local JSONL
    /// record. `--show` is read-only and also works for closed sessions.
    #[command(
        after_help = "Examples:\n  toimija handoff \"auth migration half done; run cargo test -p auth next\"\n  \
        toimija handoff --show 1782951227-52574"
    )]
    Handoff(HandoffArgs),
    /// List durable records of concluded sessions.
    ///
    /// Use this to see which agents worked in the repo after their live session
    /// directories have been reclaimed. Reads the append-only local history
    /// journal; malformed lines are counted and skipped.
    #[command(
        after_help = "Examples:\n  toimija history         # newest concluded sessions first\n  \
        toimija history --json  # all valid records as a JSON array"
    )]
    History(HistoryArgs),
    /// Reclaim expired Toimija sessions, transcripts, and orphan temp files.
    ///
    /// Use this for an explicit, auditable stewardship pass. It applies the
    /// retention settings in `.toimija/repo.yaml`, journals sessions before
    /// removing them, and reports every candidate. Unlike automatic sweeps,
    /// any error fails the command.
    #[command(
        after_help = "Examples:\n  toimija clean             # remove expired owned artifacts\n  \
        toimija clean --dry-run   # list candidates without changing disk"
    )]
    Clean(CleanArgs),
    /// Print suggested preflight text or an entry script. Writes no files.
    ///
    /// Use this to seed AGENTS.md/CLAUDE.md (`preflight`) or to save a
    /// `scripts/agent-enter` stub (`script`). Output goes to stdout only.
    Snippet(SnippetArgs),
    /// Check whether this branch's work has landed in a base ref by content.
    ///
    /// Answers "did this land?" as a content question, not an ancestry one —
    /// squash merges sever the commit DAG but preserve the tree, so a landed
    /// branch is often not an ancestor of main. Use this before deleting a
    /// branch or re-doing work that may already be merged. Read-only.
    #[command(
        after_help = "Examples:\n  toimija landed                       # did this branch land in origin/main?\n  \
        toimija landed --base origin/develop --json"
    )]
    Landed(LandedArgs),
    /// List sibling sessions live in this repo: who is here, on which branch and
    /// worktree, with what intent and scope, and where overlap warrants care.
    ///
    /// Use this before starting work in a shared repo: it reports presence
    /// (LIVE / STALE / CLOSED) and descriptive overlap warnings. Read-only
    /// unless `--prune`, which runs the same configured stewardship pass as
    /// `clean` (including expired sessions). Assigns nothing, signals nothing,
    /// no network.
    #[command(
        after_help = "Examples:\n  toimija sessions           # LIVE / STALE / CLOSED listing + warnings\n  \
        toimija sessions --json    # machine-readable report\n  \
        toimija sessions --prune   # run the configured stewardship pass"
    )]
    Sessions(SessionsArgs),
    /// Partition the committed repository tree into bounded audit worksets.
    ///
    /// Use this for breadth-scoped audits. Read-only and deterministic: it derives surfaces from Git state and
    /// points at committed prior findings without writing plans or findings.
    #[command(
        after_help = "Examples:\n  toimija audit-plan                         # list all surfaces\n  \
        toimija audit-plan --surface 3 --budget 4000   # print one workset packet"
    )]
    AuditPlan(AuditPlanArgs),
    /// Print the validation plan. Does not run any commands.
    ///
    /// Use this to see what the repo considers its fast/strict/full validation
    /// (from .toimija/repo.yaml, or inferred from the repo kind). Toimija never
    /// executes the plan — you or your agent run the commands.
    Validate(ValidateArgs),
    /// Verify the selected session or repo packet still matches live repo state.
    ///
    /// The verify-before-read gate: checks the packet's consistency fingerprint
    /// against the working tree. Inside `run`, it selects that session's packet;
    /// otherwise it selects `.toimija/current.json`. `--packet` overrides the
    /// selection. Use this before every packet read; it exits non-zero (with
    /// reasons) when the packet is stale, so a caller refreshes before trusting
    /// it. Read-only; no network.
    #[command(
        after_help = "Examples:\n  toimija verify --json || toimija refresh   # verify the ambient packet\n  \
        toimija verify --packet .toimija/current.json --json  # select explicitly"
    )]
    Verify(VerifyArgs),
    /// Materialize the working tree as an immutable git tree object.
    ///
    /// Freezes the full working state (content-addressed) so a reader can read
    /// files *at a version* via `git cat-file` instead of live disk. Prints the
    /// tree oid and the consistency token. Use this when a reader needs a
    /// stable view of a moving worktree; `--lease` pins the snapshot against
    /// `git gc` until released. Refuses in an unsafe git state. Writes git
    /// objects (gc-reclaimable); no network.
    #[command(
        after_help = "Examples:\n  toimija snapshot --lease           # freeze + pin the current worktree\n  \
        toimija snapshot --release <ID>    # let a pinned snapshot go\n  \
        toimija snapshot --list            # held leases"
    )]
    Snapshot(SnapshotArgs),
}

#[derive(Debug, Args)]
pub struct ContextArgs {
    /// Emit the packet as JSON instead of Markdown.
    #[arg(long)]
    pub json: bool,
    /// Add a diff-focused section: committed, staged, unstaged, untracked, conflicts.
    #[arg(long)]
    pub for_diff: bool,
    /// Approximate output cap in UTF-8 bytes; optional sections are omitted atomically.
    #[arg(long)]
    pub budget: Option<usize>,
}

#[derive(Debug, Args)]
pub struct AdoptArgs {
    /// Install post-checkout, post-merge, and post-commit refresh hooks.
    #[arg(long)]
    pub hooks: bool,
}

#[derive(Debug, Args)]
pub struct CurrentArgs {
    /// Synchronously write `.toimija/current.md` and `.toimija/current.json`.
    #[arg(long, conflicts_with = "verify")]
    pub write: bool,
    /// Verify `.toimija/current.json` against the live repo.
    #[arg(long)]
    pub verify: bool,
    /// Emit JSON. Required for stable verifier output consumed by harnesses.
    #[arg(long)]
    pub json: bool,
    /// Packet to verify instead of the ambient session or repo-level packet.
    #[arg(long, value_name = "PATH", requires = "verify")]
    pub packet: Option<std::path::PathBuf>,
}

#[derive(Debug, Args)]
pub struct LandedArgs {
    /// Ref the work lands in; the branch surface is compared against it.
    #[arg(long, default_value = "origin/main")]
    pub base: String,
    /// Emit the report as JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Diff-focused packet, forwarded to the entry/watch refresh.
    #[arg(long)]
    pub for_diff: bool,
    /// Output budget for the Markdown packet.
    #[arg(long)]
    pub budget: Option<usize>,
    /// Free-text declaration of why this session is here, surfaced to sibling
    /// sessions by `toimija sessions`. Advisory only — never an assignment.
    #[arg(long)]
    pub intent: Option<String>,
    /// A path this session expects to touch (repeatable). Advisory only — never a
    /// lock. Absence renders as `unknown`.
    #[arg(long = "scope", value_name = "PATH")]
    pub scope: Vec<String>,
    /// The task for the agent, delivered inside the composed launch prompt of a
    /// known agent (`claude`, `codex`, `gemini`). Pass the task here, not as the agent's
    /// own positional argument.
    #[arg(long)]
    pub task: Option<String>,
    /// The agent command to run, after `--` (e.g. `-- claude ...`).
    #[arg(trailing_var_arg = true, required = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct IntentArgs {
    /// Free-text declaration of why this session is here, shown to siblings by
    /// `toimija sessions`.
    pub text: String,
    /// A path this session expects to touch (repeatable, appended to the
    /// declared scope). Advisory only — never a lock.
    #[arg(long = "scope", value_name = "PATH")]
    pub scope: Vec<String>,
}

#[derive(Debug, Args)]
pub struct HandoffArgs {
    /// Free-text note for the next agent.
    #[arg(conflicts_with = "show")]
    pub note: Option<String>,
    /// Print recorded handoffs for a session id. Read-only; works on closed sessions.
    #[arg(long, value_name = "SESSION_ID")]
    pub show: Option<String>,
    /// Emit JSON: written record for write mode, all records array for --show.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct HistoryArgs {
    /// Emit all valid history records as a JSON array.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    /// List every expired artifact without deleting or compacting anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct SessionsArgs {
    /// Emit the report as JSON instead of the human-readable listing.
    #[arg(long)]
    pub json: bool,
    /// Run the configured stewardship pass. Live sessions are never touched.
    #[arg(long)]
    pub prune: bool,
}

#[derive(Debug, Args)]
pub struct AuditPlanArgs {
    /// Number of surfaces; defaults to a Git-derived count capped at eight.
    #[arg(long)]
    pub surfaces: Option<usize>,
    /// Print one 1-based surface workset instead of the summary.
    #[arg(long, value_name = "K")]
    pub surface: Option<usize>,
    /// Approximate output cap in UTF-8 bytes; optional sections are omitted atomically.
    #[arg(long)]
    pub budget: Option<usize>,
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Emit the verdict as JSON (`{ "consistent": bool, "reasons": [...] }`).
    #[arg(long)]
    pub json: bool,
    /// Packet to verify instead of the ambient session or repo-level packet.
    #[arg(long, value_name = "PATH")]
    pub packet: Option<std::path::PathBuf>,
}

#[derive(Debug, Args)]
pub struct SnapshotArgs {
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
    /// Pin the snapshot with a lease (a git ref) so it survives `git gc` until
    /// released. Uses the current session id when run under `toimija run`.
    #[arg(long, conflicts_with_all = ["release", "list"])]
    pub lease: bool,
    /// Release a held snapshot lease by id.
    #[arg(long, value_name = "ID", conflicts_with_all = ["lease", "list"])]
    pub release: Option<String>,
    /// List held snapshot leases.
    #[arg(long, conflicts_with_all = ["lease", "release"])]
    pub list: bool,
}

#[derive(Debug, Args)]
pub struct SnippetArgs {
    /// Which snippet to print.
    #[arg(value_enum)]
    pub kind: SnippetKind,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SnippetKind {
    /// Agent preflight text to paste into AGENTS.md/CLAUDE.md or a task prompt.
    Preflight,
    /// A `scripts/agent-enter` bash stub a human may save and run.
    Script,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Escalate every actionable warning to a failure (non-zero exit). Use as a
    /// CI or pre-commit gate so config/doc drift blocks the change.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// Print the plan only. Required: Toimija never runs validation in v0.
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

/// Dispatch the parsed command and return the process exit code. Commands that
/// wrap or gate something (`run`, `verify`) pick their own code; everything
/// else maps success to 0. Returning the code — instead of calling
/// `process::exit` inside a handler — keeps the `toimija.command.finish`
/// bookend and the span guard on every exit path.
pub fn run(cli: Cli) -> Result<i32> {
    let started = Instant::now();
    let command_name = cli.command.name();
    let output_format = match &cli.command {
        Command::Context(args) | Command::Enter(args) | Command::Refresh(args) if args.json => {
            "json"
        }
        Command::Current(args) if args.json => "json",
        Command::Handoff(args) if args.json => "json",
        Command::History(args) if args.json => "json",
        Command::Landed(args) if args.json => "json",
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
        Command::Init => run_init().map(|()| 0),
        Command::Adopt(args) => run_adopt(args).map(|()| 0),
        Command::Doctor(args) => run_doctor(args).map(|()| 0),
        Command::Context(args) => run_context(args).map(|()| 0),
        Command::Current(args) => run_current(args),
        Command::Refresh(args) => run_refresh(args).map(|()| 0),
        Command::Enter(args) => run_enter(args).map(|()| 0),
        Command::Watch(args) => run_watch(args).map(|()| 0),
        Command::Run(args) => run_run(args),
        Command::Intent(args) => run_intent(args).map(|()| 0),
        Command::Handoff(args) => run_handoff(args).map(|()| 0),
        Command::History(args) => run_history(args).map(|()| 0),
        Command::Clean(args) => run_clean(args).map(|()| 0),
        Command::Snippet(args) => run_snippet(args).map(|()| 0),
        Command::Landed(args) => run_landed(args).map(|()| 0),
        Command::Sessions(args) => run_sessions(args).map(|()| 0),
        Command::AuditPlan(args) => run_audit_plan(args).map(|()| 0),
        Command::Validate(args) => run_validate(args).map(|()| 0),
        Command::Verify(args) => run_verify(args),
        Command::Snapshot(args) => run_snapshot(args).map(|()| 0),
    };

    match &result {
        Ok(exit_code) => info!(
            event = "toimija.command.finish",
            duration_ms = started.elapsed().as_millis() as u64,
            exit_code
        ),
        // A refusal is the human's conversation, not an operational failure:
        // log a compact event instead of echoing the whole message as a WARN
        // right above where the message itself prints.
        Err(error) if error.downcast_ref::<Refusal>().is_some() => info!(
            event = "toimija.command.refused",
            duration_ms = started.elapsed().as_millis() as u64,
            reason = error.downcast_ref::<Refusal>().map(|r| r.reason)
        ),
        Err(error) => warn!(
            event = "toimija.command.finish",
            duration_ms = started.elapsed().as_millis() as u64,
            error = %error
        ),
    }

    result
}

/// A usage refusal: toimija deliberately will not proceed without more input
/// from the human. Distinct from an operational failure — it renders as a
/// friendly multi-line message (no context chain, no telemetry echo) and
/// exits 2, the usage-error convention, so scripts can tell "you asked wrong"
/// from "it broke".
#[derive(Debug)]
pub struct Refusal {
    /// Compact machine label for telemetry (e.g. `missing-intent`).
    pub reason: &'static str,
    /// The full human-facing message, already formatted for the terminal.
    pub message: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

/// Quote a word for display inside a suggested shell command, so a copied
/// suggestion still parses (`sh -c 'echo hi'`, not `sh -c echo hi`).
fn shell_word(word: &str) -> String {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@".contains(c))
    {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

fn refuse_missing_intent(command: &[String]) -> Refusal {
    let agent = command
        .iter()
        .map(|word| shell_word(word))
        .collect::<Vec<_>>()
        .join(" ");
    Refusal {
        reason: "missing-intent",
        message: format!(
            "toimija run: missing --intent — every session declares why it exists.\n\
             It is shown to sibling sessions (`toimija sessions`) and scored in the exit report.\n\
             \n\
             Try:\n\
             \x20\x20toimija run --intent \"<why>\" -- {agent}\n\
             \x20\x20toimija run --intent \"<why>\" --task \"<what to do>\" -- {agent}\n\
             \n\
             Genuinely can't say yet?\n\
             \x20\x20toimija run --intent unknown -- {agent}"
        ),
    }
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::Init => "init",
            Command::Adopt(_) => "adopt",
            Command::Doctor(_) => "doctor",
            Command::Context(_) => "context",
            Command::Current(_) => "current",
            Command::Refresh(_) => "refresh",
            Command::Enter(_) => "enter",
            Command::Watch(_) => "watch",
            Command::Run(_) => "run",
            Command::Intent(_) => "intent",
            Command::Handoff(_) => "handoff",
            Command::History(_) => "history",
            Command::Clean(_) => "clean",
            Command::Snippet(_) => "snippet",
            Command::Landed(_) => "landed",
            Command::Sessions(_) => "sessions",
            Command::AuditPlan(_) => "audit-plan",
            Command::Validate(_) => "validate",
            Command::Verify(_) => "verify",
            Command::Snapshot(_) => "snapshot",
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

fn run_adopt(args: AdoptArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;
    let report = adoption::adopt(&repo_root, args.hooks)?;
    for path in &report.files {
        println!("adopted {}", rel_display(path, &repo_root));
    }
    if report.created_agents {
        println!("created AGENTS.md because no known entry file existed");
    }
    if !report.hooks.is_empty() {
        println!("installed Git hooks: {}", report.hooks.join(", "));
        println!(
            "note: hooks live in this clone's Git hooks directory and are shared by every worktree of this clone"
        );
        if report.settings_written {
            println!("wrote .claude/settings.json (SessionStart: toimija context)");
        }
        println!(
            "note: toimija never writes config outside this repo; ~/.claude/settings.json stays yours"
        );
    }
    Ok(())
}

fn run_doctor(args: DoctorArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let mode = if args.strict {
        repo::DoctorMode::Strict
    } else {
        repo::DoctorMode::Default
    };
    let report = repo::doctor(&runner, &cwd, mode);
    println!("{}", render::render_doctor_report(&report));
    if report.serious {
        let reason = if report.strict {
            "doctor --strict found config or doc drift"
        } else {
            "doctor found serious setup failures"
        };
        anyhow::bail!("{reason}");
    }
    Ok(())
}

fn run_context(args: ContextArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let mut packet = context::build_context_packet(&runner, &cwd, args.for_diff)
        .context("failed to build context packet")?;
    packet.attach_sauma(None)?;
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

fn run_current(args: CurrentArgs) -> Result<i32> {
    if args.write {
        return run_refresh(ContextArgs {
            json: args.json,
            for_diff: false,
            budget: None,
        })
        .map(|()| 0);
    }
    if args.verify {
        // Delegates to `verify`, exit code included — harness verifiers gate
        // on it (the verify-before-read contract).
        return run_verify(VerifyArgs {
            json: args.json,
            packet: args.packet,
        });
    }
    anyhow::bail!("use `toimija current --write` or `toimija current --verify`")
}

fn run_refresh(args: ContextArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;
    let refreshed = ambient_refresh(&runner, &repo_root, &args)?;
    if args.json {
        println!("{}", refreshed.json);
    } else {
        println!(
            "Refreshed {} and {}",
            rel_display(&refreshed.md_path, &repo_root),
            rel_display(&refreshed.json_path, &repo_root)
        );
    }
    Ok(())
}

fn run_enter(args: ContextArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;
    // Enter is refresh + print: it writes the live packet, then frames it as the
    // work preflight an agent reads on entering the repo.
    let refreshed = ambient_refresh(&runner, &repo_root, &args)?;
    if args.json {
        println!("{}", refreshed.json);
    } else {
        print_agent_entry(&refreshed, &repo_root);
    }
    opportunistic_sweep(&runner, &repo_root);
    Ok(())
}

/// Refresh honoring the run handshake (issue #68): when this process runs
/// inside a `toimija run` session, target *that session's* packet — the one
/// `$TOIMIJA_LIVE` points the agent at — which also mirrors to the repo-level
/// summary (decision D3). Sessionless invocations keep writing only
/// `.toimija/current.*`. Updating one's own packet is activity, so the
/// session heartbeat is stamped too.
fn ambient_refresh(
    runner: &ProcessCommandRunner,
    repo_root: &std::path::Path,
    args: &ContextArgs,
) -> Result<refresh::Refreshed> {
    let ambient = session::ambient_session(repo_root);
    let plan = refresh::Plan {
        for_diff: args.for_diff,
        budget: args.budget,
        session_dir: ambient.as_ref().map(|(_, dir)| dir.clone()),
    };
    let refreshed = plan.run(runner, repo_root)?;
    if let Some((id, _)) = &ambient
        && let Err(error) = session::touch(repo_root, id)
    {
        warn!(event = "toimija.session.heartbeat_failed", error = %error);
    }
    Ok(refreshed)
}

/// A path as the repo-relative string used in human-facing messages; falls
/// back to the full path when it is not under the repo root.
fn rel_display(path: &std::path::Path, repo_root: &std::path::Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Frame the refreshed packet as the agent's work preflight.
fn print_agent_entry(refreshed: &refresh::Refreshed, repo_root: &std::path::Path) {
    println!(
        "Toimija agent entry — working state below (also written to {}):\n",
        rel_display(&refreshed.md_path, repo_root)
    );
    println!("{}", refreshed.markdown);
}

fn run_run(args: RunArgs) -> Result<i32> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // Proposal 0006 D6: declaring why the session exists is the entry fee of
    // the envelope. Refused before any session state is touched.
    if args.intent.is_none() {
        return Err(refuse_missing_intent(&args.command).into());
    }

    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;

    // 1. Open a session: its own packet directory + marker, so concurrent
    //    wrapped agents do not race on the single repo-level packet.
    let mut session = session::Session::open(
        &repo_root,
        &args.command,
        args.intent.clone(),
        args.scope.clone(),
    )
    .context("failed to open the toimija session")?;

    // 2. Enter: refresh the session packet (authoritative for the agent, and
    //    mirrored to the repo-level summary) and print it as the work preflight.
    //    The plan (session-targeted) is reused by the watch loop and the exit.
    let plan = refresh::Plan {
        for_diff: args.for_diff,
        budget: args.budget,
        session_dir: Some(session.dir.clone()),
    };
    let entry = match plan.run(&runner, &repo_root) {
        Ok(entry) => entry,
        Err(error) => {
            close_session_after_startup_failure(&repo_root, &mut session);
            return Err(error);
        }
    };
    let entry = match verify_run_entry(&runner, &cwd, &repo_root, &plan, entry, &session.id) {
        Ok(entry) => entry,
        Err(error) => {
            close_session_after_startup_failure(&repo_root, &mut session);
            return Err(error);
        }
    };
    print_agent_entry(&entry, &repo_root);
    // The branch and HEAD are known once the packet is built; record them.
    if let Err(error) = session.record_git(
        entry.packet.git.branch.clone(),
        entry.packet.git.head_oid.clone(),
    ) {
        close_session_after_startup_failure(&repo_root, &mut session);
        return Err(error);
    }

    // 3. Start watching synchronously, so a watcher startup failure is reported
    //    here — before launching the command — instead of being silently lost.
    let watch_session = match watch::start(&repo_root)
        .context("failed to start the file watcher for `run` (try `toimija enter` then run the command directly)")
    {
        Ok(watch_session) => watch_session,
        Err(error) => {
            close_session_after_startup_failure(&repo_root, &mut session);
            return Err(error);
        }
    };
    if let Err(error) = watch_session.catch_up(&runner, &repo_root, &plan) {
        close_session_after_startup_failure(&repo_root, &mut session);
        return Err(error).context("failed to refresh after arming the file watcher");
    }
    eprintln!(
        "toimija run: watcher armed ({})",
        watch_session.backend_label()
    );
    opportunistic_sweep(&runner, &repo_root);

    // 4. Run the watch loop in the background, refreshing the SESSION packet
    //    while the agent works. The stop flag is flipped once the command exits.
    let stop = Arc::new(AtomicBool::new(false));
    let watcher_stop = Arc::clone(&stop);
    // Count how many times the packet refreshed while the agent worked, so the
    // exit summary can report it. The closure is the only place `run` reacts to
    // a refresh — it stays silent on screen (the wrapped command owns the
    // terminal) and just bumps the counter.
    let refreshes = Arc::new(AtomicUsize::new(0));
    let watcher_refreshes = Arc::clone(&refreshes);
    let watch_root = repo_root.clone();
    let watch_plan = plan.clone();
    let heartbeat_id = session.id.clone();
    let watcher = std::thread::spawn(move || {
        let runner = ProcessCommandRunner;
        watch_session.run(&runner, &watch_root, &watch_plan, &watcher_stop, || {
            watcher_refreshes.fetch_add(1, Ordering::Relaxed);
            // Heartbeat (proposal 0002): an event worth a refresh is worth a
            // `last_seen_at` stamp, so staleness measures inactivity rather
            // than lifetime. Best-effort — never interrupts the watch loop.
            if let Err(error) = session::touch(&watch_root, &heartbeat_id) {
                warn!(event = "toimija.session.heartbeat_failed", error = %error);
            }
        })
    });

    // 5. Launch the agent command, inheriting the terminal. The child also
    //    inherits the per-session handshake (`TOIMIJA_RUN`, `TOIMIJA_SESSION`,
    //    `TOIMIJA_LIVE*`, `TOIMIJA_REPO_LIVE`), so a wrapped agent can *discover*
    //    its own continuously-refreshed packet instead of hardcoding a path.
    let (program, rest) = args
        .command
        .split_first()
        .expect("clap `required = true` guarantees at least one argument");
    // Proposal 0006 Part C: for a known agent the packet travels in argv — in
    // context by construction. Anything else keeps the env-var handshake, and
    // says so instead of silently degrading.
    let live_packet = session.dir.join("current.md");
    let composed = crate::adapters::agents::compose_launch_prompt(
        program,
        &entry.markdown,
        &live_packet,
        args.task.as_deref(),
    );
    match &composed {
        Some(_) => eprintln!(
            "toimija run: delivery: launch prompt with the packet inlined for `{program}`"
        ),
        None => {
            eprintln!("toimija run: delivery: env-var only — not guaranteed for `{program}`");
            if args.task.is_some() {
                eprintln!(
                    "toimija run: --task travels in the composed launch prompt and `{program}` \
                     is not a known agent; give it its task yourself"
                );
            }
        }
    }
    eprintln!(
        "toimija run: launching `{program}` (session {}, watching for changes)",
        session.id
    );
    let mut child = std::process::Command::new(program);
    child.args(rest);
    if let Some(prompt) = &composed {
        child.arg(prompt);
    }
    let launched = child
        .current_dir(&cwd)
        .envs(session.env(&repo_root))
        .status();

    // 6. Stop the watcher and join, whatever happened to the command. A loop
    //    error (e.g. a mid-run refresh failure) is reported, but the wrapped
    //    command's status still wins the exit code — the child has already run.
    stop.store(true, Ordering::Relaxed);
    match watcher.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("toimija run: watcher stopped on error: {error:#}"),
        // The default panic hook already printed message+location from inside
        // the thread; still, include the payload here so the join site is not
        // silent about *what* panicked.
        Err(payload) => eprintln!(
            "toimija run: watcher thread panicked: {}",
            panic_message(payload.as_ref())
        ),
    }

    // 7. Close the session marker (auto-close on exit), then bookend the session:
    //    recompute the packet one last time (leaving both the session and
    //    repo-level packets in their final state) and print a summary diffing the
    //    Git state from entry to exit. Failures here are non-fatal — the wrapped
    //    command already ran; we just skip that step.
    // Record the wrapped command's exit code on the marker when it launched;
    // peeked (not consumed) so the final `?` below still owns the status.
    let exit_status = launched
        .as_ref()
        .ok()
        .map(|status| wrapped_exit_code(*status));
    match session.close(exit_status) {
        Ok(()) => journal_closed_session(&repo_root, &session),
        Err(error) => eprintln!(
            "toimija run: could not close the session marker; it stays 'active' on disk \
             and may show as a live session. Remove {}: {error:#}",
            session.dir.join("session.json").display(),
        ),
    }
    // Skip the exit summary if the command never launched — there is nothing to
    // summarize, and the launch error is surfaced below.
    if launched.is_ok() {
        let consumption = collect_consumption(&repo_root, &session, &args);
        match plan.run(&runner, &repo_root) {
            Ok(exit) => eprint!(
                "\n{}",
                render_run_summary(
                    &entry.packet.git,
                    &exit.packet.git,
                    refreshes.load(Ordering::Relaxed),
                    &consumption,
                )
            ),
            Err(error) => eprintln!("toimija run: could not compute exit summary: {error:#}"),
        }
    }

    // 8. Exit with the wrapped command's status — preserving signal semantics so
    //    a cancelled/terminated child is distinguishable from a generic failure.
    //    Returned (not `process::exit`) so the command.finish bookend still runs.
    let status = launched.with_context(|| format!("failed to launch `{program}`"))?;
    Ok(wrapped_exit_code(status))
}

fn verify_run_entry(
    runner: &impl crate::command::CommandRunner,
    cwd: &std::path::Path,
    repo_root: &std::path::Path,
    plan: &refresh::Plan,
    mut entry: refresh::Refreshed,
    session_id: &str,
) -> Result<refresh::Refreshed> {
    for attempt in 0..=1 {
        let (mut live, _warnings) = git::collect_git_state(runner, cwd)
            .context("failed to verify the `run` entry packet")?;
        context::normalize_git_state(&mut live);
        match entry.packet.consistency.verify(&live) {
            Verdict::Consistent => {
                info!(event = "toimija.run.packet_verified", session = %session_id);
                return Ok(entry);
            }
            Verdict::Stale {
                packet_token,
                current_token,
            } if attempt == 0 => {
                warn!(
                    event = "toimija.run.packet_raced",
                    session = %session_id,
                    packet_token = %packet_token,
                    current_token = %current_token,
                    "refreshing once before launch"
                );
                entry = plan
                    .run(runner, repo_root)
                    .context("failed to refresh the raced `run` entry packet")?;
            }
            Verdict::Stale { .. } => anyhow::bail!(
                "refusing to launch: the `run` entry packet changed again after one refresh"
            ),
            Verdict::UnsupportedConsistencyAlgorithm(algorithm) => anyhow::bail!(
                "refusing to launch: unsupported entry consistency algorithm `{algorithm}`"
            ),
            Verdict::UnsafeGitOperation(operation) => anyhow::bail!(
                "refusing to launch in unsafe Git state: {operation:?} ({} conflicted file(s))",
                live.conflicted_files.len()
            ),
        }
    }
    unreachable!("the two-attempt entry gate always returns or errors")
}

fn close_session_after_startup_failure(
    repo_root: &std::path::Path,
    session: &mut session::Session,
) {
    // The wrapped command never launched, so there is no exit status to record.
    match session.close(None) {
        Ok(()) => journal_closed_session(repo_root, session),
        Err(error) => {
            eprintln!("toimija run: could not close the failed session marker: {error:#}")
        }
    }
}

fn journal_closed_session(repo_root: &std::path::Path, session: &session::Session) {
    let result = (|| {
        let marker = session::load_marker(&session.dir)?;
        footprint::journal_session(repo_root, &marker, HistoryEnd::Closed, now_secs())?;
        Result::<()>::Ok(())
    })();
    if let Err(error) = result {
        warn!(
            event = "toimija.footprint.journal_failed",
            session = %session.id,
            error = %error
        );
    }
}

/// What the session consumed of toimija — and nothing else. Proposal 0006 D7:
/// built only from toimija-owned state (the session marker and handoff
/// records); nothing observed about the agent's behavior or file access.
struct Consumption {
    intent: String,
    intent_source: &'static str,
    expansions: usize,
    handoffs: usize,
}

fn collect_consumption(
    repo_root: &std::path::Path,
    session: &session::Session,
    args: &RunArgs,
) -> Consumption {
    let marker = session::load_marker(&session.dir).ok();
    let launch_intent = args.intent.clone().unwrap_or_default();
    let (intent, intent_source) = match marker.as_ref().and_then(|m| m.intent.clone()) {
        Some(text) if text == launch_intent => (text, "declared at launch"),
        Some(text) => (text, "updated mid-session"),
        None => (launch_intent, "declared at launch"),
    };
    let expansions = marker
        .map(|m| m.scope.len().saturating_sub(args.scope.len()))
        .unwrap_or(0);
    let handoffs = handoff::count(repo_root, &session.id).unwrap_or(0);
    Consumption {
        intent,
        intent_source,
        expansions,
        handoffs,
    }
}

/// Bookend a `run` session: a compact, stderr-bound summary of what the wrapped
/// command did to the repo, by diffing the Git state captured on entry against
/// the state at exit. Only the deltas worth a glance are shown — a switched
/// branch, a moved HEAD (commits made), the working tree at exit, branch scope
/// since the merge-base, how many times the live packet refreshed, and what
/// the session consumed of toimija (intent, declared expansions, handoff).
fn render_run_summary(
    entry: &git::GitState,
    exit: &git::GitState,
    refreshes: usize,
    consumption: &Consumption,
) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    let _ = writeln!(out, "toimija run: session summary");

    // Branch — flag a switch, since `run` happily wraps a session that checks
    // out a different branch midway.
    match (&entry.branch, &exit.branch) {
        (Some(before), Some(after)) if before != after => {
            let _ = writeln!(out, "  branch:    {before} → {after}");
        }
        (_, Some(now)) => {
            let _ = writeln!(out, "  branch:    {now}");
        }
        _ => {}
    }

    // HEAD — did commits land while the session ran?
    if let (Some(before), Some(after)) = (&entry.head_oid, &exit.head_oid)
        && before != after
    {
        let _ = writeln!(
            out,
            "  HEAD:      {} → {} (moved)",
            short_oid(before),
            short_oid(after)
        );
    }

    // Working tree as the session leaves it.
    let conflicts = if exit.conflicted_files.is_empty() {
        String::new()
    } else {
        format!(", {} conflicted", exit.conflicted_files.len())
    };
    let _ = writeln!(
        out,
        "  worktree:  {} staged, {} unstaged, {} untracked{conflicts}",
        exit.staged_files.len(),
        exit.unstaged_files.len(),
        exit.untracked_files.len(),
    );

    let _ = writeln!(
        out,
        "  scope:     {} file(s) changed on this branch since merge-base",
        exit.branch_files.len(),
    );
    let _ = writeln!(out, "  refreshed: {refreshes} time(s) while you worked");

    let _ = writeln!(
        out,
        "  intent:    \"{}\" ({})",
        consumption.intent, consumption.intent_source
    );
    if consumption.expansions == 0 {
        let _ = writeln!(out, "  expansions: none declared");
    } else {
        let _ = writeln!(
            out,
            "  expansions: {} scope path(s) declared while working",
            consumption.expansions
        );
    }
    if consumption.handoffs == 0 {
        let _ = writeln!(out, "  handoff:   none recorded");
    } else {
        let _ = writeln!(out, "  handoff:   {} record(s)", consumption.handoffs);
    }
    out
}

/// Short form of a Git OID for display, robust to already-short or empty input.
fn short_oid(oid: &str) -> &str {
    &oid[..oid.len().min(7)]
}

/// The exit code a transparent wrapper should adopt for a finished child. On
/// Unix a signal-terminated child has no exit code; follow the shell convention
/// of `128 + signal` (e.g. SIGTERM → 143) so termination is not flattened to 1.
#[cfg(unix)]
fn wrapped_exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

#[cfg(not(unix))]
fn wrapped_exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

/// Best-effort human message from a joined thread's panic payload. `panic!`
/// with a literal carries `&str`; with a formatted message, `String`; anything
/// else is opaque.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>")
}

fn run_watch(args: ContextArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;
    // Status goes to stderr so stdout stays clean for any piping.
    eprintln!(
        "toimija watch: refreshing {} on change (Ctrl-C to stop)",
        refresh::CURRENT_MD
    );
    // Standalone watch runs until the process is signalled (Ctrl-C). The flag
    // exists to satisfy the shared `watch` signature and is never set here;
    // `run` drives its own stop flag, not this one.
    let stop = std::sync::atomic::AtomicBool::new(false);
    // Standalone watch is sessionless: it refreshes only the repo-level packet.
    let plan = refresh::Plan {
        for_diff: args.for_diff,
        budget: args.budget,
        session_dir: None,
    };
    watch::watch(&runner, &repo_root, &plan, &stop, || {
        eprintln!(
            "refreshed {} / {}",
            refresh::CURRENT_MD,
            refresh::CURRENT_JSON
        );
    })?;
    Ok(())
}

fn run_landed(args: LandedArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;
    let report = landed::assess_landing(&runner, &repo_root, &args.base);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", render::render_landed_report(&report));
    }
    Ok(())
}

fn run_snapshot(args: SnapshotArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;

    // List held leases.
    if args.list {
        let leases = snapshot::list_leases(&repo_root)?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&leases)?);
        } else if leases.is_empty() {
            println!("no snapshot leases");
        } else {
            for lease in &leases {
                println!(
                    "{}  tree {}  {}  pid {}",
                    lease.id,
                    short_oid(&lease.tree),
                    lease.token,
                    lease.pid
                );
            }
        }
        return Ok(());
    }

    // Release a held lease.
    if let Some(id) = &args.release {
        snapshot::release_lease(&runner, &repo_root, id)?;
        println!("released snapshot lease {id}");
        return Ok(());
    }

    // Materialize only a *safe* state: `write-tree` should never freeze a
    // half-finished merge/rebase or a conflicted tree. Same gate as verify.
    let (mut live, _warnings) = git::collect_git_state(&runner, &cwd)?;
    context::normalize_git_state(&mut live);
    let consistency = Consistency::fingerprint(&live);
    if consistency.git_operation != GitOperationState::None {
        anyhow::bail!(
            "refusing to snapshot: repository is in an unsafe git operation ({:?}); finish or abort it first",
            consistency.git_operation
        );
    }
    if consistency.conflict_state.has_conflicts {
        anyhow::bail!("refusing to snapshot: repository has unresolved conflicts");
    }
    // A path that is both staged and modified in the worktree has different
    // content in the index (staged) than on disk (worktree). A single tree can
    // hold only one, so "the snapshot at this token" would be ambiguous — the
    // token fingerprints both index and worktree, the tree cannot. Refuse.
    let staged: std::collections::HashSet<&std::path::PathBuf> =
        live.staged_files.iter().map(|file| &file.path).collect();
    let mixed: Vec<String> = live
        .unstaged_files
        .iter()
        .filter(|file| staged.contains(&file.path))
        .map(|file| file.path.display().to_string())
        .collect();
    if !mixed.is_empty() {
        anyhow::bail!(
            "refusing to snapshot: {} path(s) are both staged and modified in the worktree ({}); stage or discard the worktree changes so one tree can represent the state",
            mixed.len(),
            mixed.join(", ")
        );
    }
    let tree = snapshot::materialize(&runner, &repo_root)?;

    if args.lease {
        // Tie the lease to the session when wrapped by `toimija run`; otherwise
        // mint a standalone id.
        let id = std::env::var("TOIMIJA_SESSION")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_secs())
                    .unwrap_or(0);
                format!("{secs}-{}", std::process::id())
            });
        let lease = snapshot::acquire_lease(&runner, &repo_root, &id, &tree, &consistency.token)?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&lease)?);
        } else {
            println!("leased snapshot {}", lease.id);
            println!("  tree:  {}", lease.tree);
            println!("  token: {}", lease.token);
            println!("  ref:   {}", snapshot::lease_ref(&lease.id));
        }
    } else if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "tree": tree,
                "token": consistency.token,
                "algorithm": consistency.algorithm,
                "head": consistency.head,
            }))?
        );
    } else {
        println!("snapshot tree: {tree}");
        println!("token: {}", consistency.token);
    }
    Ok(())
}

fn run_verify(args: VerifyArgs) -> Result<i32> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let report = compute_verify_status(&runner, &cwd, args.packet.as_deref());
    let verified = matches!(report.status, VerifyStatus::Verified { .. });
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report.to_json())?);
    } else {
        match &report.status {
            VerifyStatus::Verified { .. } => println!(
                "verified: {} matches the live repo",
                report
                    .packet_path
                    .as_deref()
                    .map_or_else(|| "packet".to_string(), |path| path.display().to_string())
            ),
            other => eprintln!("{}", other.message()),
        }
    }

    // Exit non-zero on staleness so a caller can gate on the packet before
    // trusting it (the verify-before-read contract).
    Ok(if verified { 0 } else { 1 })
}

/// Collect the live Git state and resolve a verify status. A repository that
/// cannot be discovered or read is itself a refusal state
/// (`unverifiable_repository`), not a hard error — the harness needs the verdict,
/// not a panic.
fn compute_verify_status(
    runner: &ProcessCommandRunner,
    cwd: &std::path::Path,
    explicit_packet: Option<&std::path::Path>,
) -> VerifyReport {
    let repo_root = match git::discover_repo_root(runner, cwd) {
        Ok(root) => root,
        Err(error) => {
            return VerifyReport {
                packet_path: None,
                status: VerifyStatus::UnverifiableRepository {
                    message: format!("not a Git repository: {error}"),
                },
            };
        }
    };
    let packet_path = match select_verify_packet(&repo_root, cwd, explicit_packet) {
        Ok(path) => path,
        Err(message) => {
            return VerifyReport {
                packet_path: explicit_packet.map(std::path::Path::to_path_buf),
                status: VerifyStatus::InvalidPacket { message },
            };
        }
    };
    // Recompute the live fingerprint through the same path the packet build uses
    // (collect -> normalize), so an unchanged repo verifies as consistent.
    let mut live = match git::collect_git_state(runner, cwd) {
        Ok((live, _warnings)) => live,
        Err(error) => {
            return VerifyReport {
                packet_path: Some(packet_path),
                status: VerifyStatus::UnverifiableRepository {
                    message: format!("could not read Git state: {error}"),
                },
            };
        }
    };
    context::normalize_git_state(&mut live);
    let status = verify_status(&packet_path, &live);
    VerifyReport {
        packet_path: Some(packet_path),
        status,
    }
}

fn verify_status(packet_path: &std::path::Path, live: &git::GitState) -> VerifyStatus {
    match load_stored_packet(packet_path) {
        StoredPacket::Ready { packet, raw } => {
            if packet.consistency.algorithm != CONSISTENCY_ALGORITHM {
                return VerifyStatus::UnsupportedConsistencyAlgorithm {
                    algorithm: packet.consistency.algorithm,
                };
            }
            match crate::sauma::verify_value(*raw) {
                Ok(Some((expected, actual))) => {
                    return VerifyStatus::SaumaDigestMismatch { expected, actual };
                }
                Ok(None) => {}
                Err(error) => {
                    return VerifyStatus::InvalidPacket {
                        message: error.to_string(),
                    };
                }
            }
            let artifact_digest = packet.sauma.as_ref().map(|sauma| sauma.digest.clone());
            let stored = packet.consistency;
            match stored.verify(live) {
                Verdict::Consistent => VerifyStatus::Verified {
                    algorithm: stored.algorithm,
                    token: stored.token,
                    head: stored.head,
                    artifact_digest,
                },
                Verdict::Stale {
                    packet_token,
                    current_token,
                } => VerifyStatus::StalePacket {
                    packet_token,
                    current_token,
                },
                Verdict::UnsupportedConsistencyAlgorithm(algorithm) => {
                    VerifyStatus::UnsupportedConsistencyAlgorithm { algorithm }
                }
                Verdict::UnsafeGitOperation(operation) => VerifyStatus::UnsafeGitOperation {
                    operation,
                    message: "Repository is in an unsafe Git operation. Finish or abort it before running an agent harness.".to_string(),
                },
            }
        }
        StoredPacket::MissingPacket => VerifyStatus::MissingPacket {
            path: packet_path.display().to_string(),
        },
        StoredPacket::InvalidPacket(message) => VerifyStatus::InvalidPacket { message },
        StoredPacket::MissingConsistency => VerifyStatus::MissingConsistency,
        StoredPacket::IoError(message) => VerifyStatus::IoError { message },
    }
}

/// Choose the exact packet the caller will read. An explicit path wins; inside
/// `toimija run`, the validated session handshake wins; otherwise verification
/// falls back to the repo-level packet.
fn select_verify_packet(
    repo_root: &std::path::Path,
    cwd: &std::path::Path,
    explicit: Option<&std::path::Path>,
) -> std::result::Result<std::path::PathBuf, String> {
    if let Some(path) = explicit {
        return validate_packet_path(repo_root, cwd, path);
    }
    if let Some((_id, session_dir)) = session::ambient_session(repo_root) {
        let expected = normalize_absolute(&session_dir.join("current.json"))
            .ok_or_else(|| "invalid ambient session packet path".to_string())?;
        if let Some(path) = std::env::var_os("TOIMIJA_LIVE_JSON") {
            let selected = validate_packet_path(repo_root, cwd, std::path::Path::new(&path))?;
            if selected != expected {
                return Err(format!(
                    "TOIMIJA_LIVE_JSON points at {}, but session packet is {}",
                    selected.display(),
                    expected.display()
                ));
            }
        }
        return Ok(expected);
    }
    Ok(repo_root.join(refresh::CURRENT_JSON))
}

fn validate_packet_path(
    repo_root: &std::path::Path,
    cwd: &std::path::Path,
    path: &std::path::Path,
) -> std::result::Result<std::path::PathBuf, String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let path = normalize_absolute(&path)
        .ok_or_else(|| format!("invalid packet path: {}", path.display()))?;
    let repo_packet = repo_root.join(refresh::CURRENT_JSON);
    let allowed = if path == repo_packet {
        true
    } else {
        path.strip_prefix(repo_root.join(session::SESSIONS_SUBDIR))
            .ok()
            .and_then(|relative| {
                let mut parts = relative.components();
                let id = parts.next()?.as_os_str().to_str()?;
                let file = parts.next()?.as_os_str();
                (session::is_valid_session_id(id)
                    && file == "current.json"
                    && parts.next().is_none()
                    && repo_root
                        .join(session::SESSIONS_SUBDIR)
                        .join(id)
                        .join("session.json")
                        .is_file())
                .then_some(())
            })
            .is_some()
    };
    if !allowed {
        return Err(format!(
            "packet path must be {} or a session current.json under {}",
            repo_packet.display(),
            repo_root.join(session::SESSIONS_SUBDIR).display()
        ));
    }
    if path.exists() {
        let canonical = std::fs::canonicalize(&path)
            .map_err(|error| format!("failed to resolve {}: {error}", path.display()))?;
        let canonical_root = std::fs::canonicalize(repo_root)
            .map_err(|error| format!("failed to resolve {}: {error}", repo_root.display()))?;
        if !canonical.starts_with(canonical_root) {
            return Err(format!(
                "packet path escapes the repository: {}",
                path.display()
            ));
        }
        return Ok(canonical);
    }
    Ok(path)
}

fn normalize_absolute(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Some(normalized)
}

struct VerifyReport {
    packet_path: Option<std::path::PathBuf>,
    status: VerifyStatus,
}

impl VerifyReport {
    fn to_json(&self) -> serde_json::Value {
        let mut value = self.status.to_json();
        if let (Some(path), serde_json::Value::Object(map)) = (&self.packet_path, &mut value) {
            map.insert(
                "packet_path".to_string(),
                serde_json::Value::String(path.display().to_string()),
            );
        }
        value
    }
}

enum StoredPacket {
    Ready {
        packet: Box<context::ContextPacket>,
        raw: Box<serde_json::Value>,
    },
    MissingPacket,
    InvalidPacket(String),
    MissingConsistency,
    IoError(String),
}

fn load_stored_packet(path: &std::path::Path) -> StoredPacket {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return StoredPacket::MissingPacket;
        }
        Err(error) => return StoredPacket::IoError(error.to_string()),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(error) => return StoredPacket::InvalidPacket(error.to_string()),
    };
    if value.get("consistency").is_none() {
        return StoredPacket::MissingConsistency;
    }
    let packet: context::ContextPacket = match serde_json::from_value(value.clone()) {
        Ok(packet) => packet,
        Err(error) => return StoredPacket::InvalidPacket(error.to_string()),
    };
    StoredPacket::Ready {
        packet: Box::new(packet),
        raw: Box::new(value),
    }
}

#[derive(serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum VerifyStatus {
    Verified {
        algorithm: String,
        token: String,
        head: crate::consistency::HeadState,
        artifact_digest: Option<String>,
    },
    MissingPacket {
        path: String,
    },
    InvalidPacket {
        message: String,
    },
    MissingConsistency,
    UnsupportedConsistencyAlgorithm {
        algorithm: String,
    },
    StalePacket {
        packet_token: String,
        current_token: String,
    },
    SaumaDigestMismatch {
        expected: String,
        actual: String,
    },
    UnsafeGitOperation {
        operation: GitOperationState,
        message: String,
    },
    UnverifiableRepository {
        message: String,
    },
    IoError {
        message: String,
    },
}

impl VerifyStatus {
    fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("verify status serializes");
        if let serde_json::Value::Object(map) = &mut value {
            match self {
                VerifyStatus::Verified { .. } => {
                    map.entry("algorithm").or_insert_with(|| {
                        serde_json::Value::String(CONSISTENCY_ALGORITHM.to_string())
                    });
                }
                VerifyStatus::StalePacket { .. } => {
                    map.insert(
                        "message".to_string(),
                        serde_json::Value::String(
                            "The repository changed after the selected packet was generated."
                                .to_string(),
                        ),
                    );
                }
                VerifyStatus::MissingConsistency => {
                    map.insert(
                        "message".to_string(),
                        serde_json::Value::String(
                            "The selected packet does not contain a consistency block.".to_string(),
                        ),
                    );
                }
                _ => {}
            }
        }
        value
    }

    fn message(&self) -> String {
        match self {
            VerifyStatus::Verified { .. } => "verified".to_string(),
            VerifyStatus::MissingPacket { path } => {
                format!(
                    "missing_packet: no verifiable packet at {path}; run `toimija current --write`"
                )
            }
            VerifyStatus::InvalidPacket { message } => format!("invalid_packet: {message}"),
            VerifyStatus::MissingConsistency => {
                "missing_consistency: run `toimija current --write`".to_string()
            }
            VerifyStatus::UnsupportedConsistencyAlgorithm { algorithm } => {
                format!("unsupported_consistency_algorithm: {algorithm}")
            }
            VerifyStatus::StalePacket { .. } => {
                "stale_packet: repository changed after the selected packet was generated"
                    .to_string()
            }
            VerifyStatus::SaumaDigestMismatch { .. } => {
                "sauma_digest_mismatch: selected packet was tampered with".to_string()
            }
            VerifyStatus::UnsafeGitOperation { message, .. } => {
                format!("unsafe_git_operation: {message}")
            }
            VerifyStatus::UnverifiableRepository { message } => {
                format!("unverifiable_repository: {message}")
            }
            VerifyStatus::IoError { message } => format!("io_error: {message}"),
        }
    }
}

fn run_sessions(args: SessionsArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;
    if args.prune {
        let config = RepoConfig::load_optional(&repo_root)?
            .unwrap_or_else(|| RepoConfig::default_for(&repo_root));
        let report = footprint::sweep(
            &runner,
            &repo_root,
            sweep_options(&config, false),
            footprint::ErrorPosture::Explicit,
        )?
        .expect("explicit sweep always returns a report");
        if args.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print!("{}", footprint::render_sweep(&report));
        }
        return Ok(());
    }
    let report = sessions::report(&runner, &repo_root, sessions::DEFAULT_STALE_AFTER_SECS)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", sessions::render_text(&report));
    }
    Ok(())
}

fn run_clean(args: CleanArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;
    let config = RepoConfig::load_optional(&repo_root)?
        .unwrap_or_else(|| RepoConfig::default_for(&repo_root));
    let report = footprint::sweep(
        &runner,
        &repo_root,
        sweep_options(&config, args.dry_run),
        footprint::ErrorPosture::Explicit,
    )?
    .expect("explicit sweep always returns a report");
    print!("{}", footprint::render_sweep(&report));
    Ok(())
}

fn opportunistic_sweep(runner: &ProcessCommandRunner, repo_root: &std::path::Path) {
    let config = match RepoConfig::load_optional(repo_root) {
        Ok(Some(config)) => config,
        Ok(None) => RepoConfig::default_for(repo_root),
        Err(error) => {
            warn!(event = "toimija.footprint.sweep_failed", error = %error);
            return;
        }
    };
    if !config.stewardship.auto_sweep {
        return;
    }
    if let Err(error) = footprint::sweep(
        runner,
        repo_root,
        sweep_options(&config, false),
        footprint::ErrorPosture::Opportunistic,
    ) {
        warn!(event = "toimija.footprint.sweep_failed", error = %error);
    }
}

fn sweep_options(config: &RepoConfig, dry_run: bool) -> footprint::SweepOptions {
    footprint::SweepOptions {
        dry_run,
        sessions_retention_secs: config.stewardship.sessions_retention_days * config::DAY_SECS,
        runs_retention_secs: config.stewardship.runs_retention_days * config::DAY_SECS,
        history_max_records: config.stewardship.history_max_records,
    }
}

fn run_audit_plan(args: AuditPlanArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let plan =
        audit_plan::build(&runner, &cwd, args.surfaces).context("failed to build audit plan")?;
    if let Some(surface) = args.surface
        && (surface == 0 || surface > plan.surfaces.len())
    {
        anyhow::bail!(
            "surface {surface} is out of range; valid range is 1..={}",
            plan.surfaces.len()
        );
    }
    let budget = args.budget.unwrap_or(plan.default_budget);
    info!(
        event = "toimija.audit_plan.rendered",
        surfaces = plan.surfaces.len(),
        surface = args.surface.unwrap_or(0),
        findings = plan.prior_findings.len()
    );
    println!("{}", render::render_audit_plan(&plan, budget, args.surface));
    Ok(())
}

fn run_intent(args: IntentArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;
    // The wrapped agent's own session id, injected by `run`'s env handshake.
    let id = std::env::var("TOIMIJA_SESSION").map_err(|_| {
        anyhow::anyhow!(
            "TOIMIJA_SESSION is not set — `toimija intent` speaks for a wrapped session, \
             so run it from inside `toimija run -- <agent>` (the handshake provides the id)"
        )
    })?;
    let marker = session::declare_intent(&repo_root, &id, args.text, args.scope)?;
    info!(event = "toimija.session.intent_declared", session = %marker.id);
    println!(
        "session {}: intent — {}",
        marker.id,
        marker.intent.as_deref().unwrap_or("unknown")
    );
    if !marker.scope.is_empty() {
        println!("  scope: {}", marker.scope.join(", "));
    }
    Ok(())
}

fn run_handoff(args: HandoffArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;
    if let Some(id) = args.show {
        let records = handoff::read_all(&repo_root, &id)?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&records)?);
        } else {
            print_handoff_records(&id, &records)?;
        }
        return Ok(());
    }

    let id = std::env::var("TOIMIJA_SESSION").map_err(|_| {
        anyhow::anyhow!(
            "TOIMIJA_SESSION is not set — `toimija handoff` writes for a wrapped session, \
             so run it from inside `toimija run -- <agent>` (the handshake provides the id)"
        )
    })?;
    let record = handoff::write(&runner, &repo_root, &id, args.note)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&record)?);
    } else {
        println!(
            "Wrote handoff for session {} at {}",
            record.session_id, record.written_at
        );
    }
    Ok(())
}

fn run_history(args: HistoryArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;
    let history = footprint::read_history(&repo_root)?;
    info!(
        event = "toimija.history.shown",
        records = history.records.len(),
        malformed = history.malformed
    );
    if args.json {
        println!("{}", serde_json::to_string_pretty(&history.records)?);
    } else {
        print!("{}", footprint::render_history(&history));
    }
    Ok(())
}

fn print_handoff_records(id: &str, records: &[handoff::HandoffRecord]) -> Result<()> {
    anyhow::ensure!(!records.is_empty(), "no handoff recorded for session {id}");
    let latest = records.last().expect("checked non-empty");
    let age = now_secs().saturating_sub(latest.written_at);
    println!(
        "Handoff for session {id}: {} record(s), latest written {} ago",
        records.len(),
        humanize_age(age)
    );
    println!("  written_at: {}", latest.written_at);
    if let Some(step) = &latest.next_step {
        println!("  next_step: {step}");
    }
    if !latest.suggested_validation.is_empty() {
        println!(
            "  suggested_validation: {}",
            latest.suggested_validation.join("; ")
        );
    }
    println!("  consistency_token: {}", latest.consistency_token);
    Ok(())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn humanize_age(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

fn run_snippet(args: SnippetArgs) -> Result<()> {
    let snippet = match args.kind {
        SnippetKind::Preflight => snippet::Snippet::Preflight,
        SnippetKind::Script => snippet::Snippet::Script,
    };
    // The text already ends in a newline; print it verbatim so it pastes clean.
    print!("{}", snippet.text());
    Ok(())
}

fn run_validate(args: ValidateArgs) -> Result<()> {
    if !args.plan {
        anyhow::bail!("`toimija validate` only supports `--plan`; it never runs validation");
    }
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;
    let config = RepoConfig::load_optional(&repo_root)?
        .unwrap_or_else(|| RepoConfig::default_for(&repo_root));
    let kind = repo::detect_repo_kind(&repo_root);
    // Intentionally surface-independent: `validate --plan` answers "what does
    // this repo's validation look like?", so it renders the static plan only.
    // The `Focused (from changed surface)` tier is a property of a diff and is
    // enriched in `context::build_context_packet`; it is absent here by design.
    let plan = validate::validation_plan(&repo_root, &config, kind);
    println!("{}", render::render_validation_plan(&plan));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitState;
    use std::path::PathBuf;

    #[test]
    fn panic_message_recovers_str_and_string_payloads() {
        let literal = std::panic::catch_unwind(|| panic!("literal payload")).unwrap_err();
        assert_eq!(panic_message(literal.as_ref()), "literal payload");

        let formatted = std::panic::catch_unwind(|| panic!("formatted {}", "payload")).unwrap_err();
        assert_eq!(panic_message(formatted.as_ref()), "formatted payload");

        let opaque = std::panic::catch_unwind(|| std::panic::panic_any(42_u8)).unwrap_err();
        assert_eq!(panic_message(opaque.as_ref()), "<non-string panic payload>");
    }

    /// A minimal `GitState` for summary tests: clean tree on `branch` at `head`.
    fn state(branch: &str, head: &str) -> GitState {
        GitState {
            repo_root: PathBuf::from("/repo"),
            git_dir: None,
            branch: Some(branch.to_string()),
            head_oid: Some(head.to_string()),
            upstream: None,
            base: None,
            merge_base: None,
            detached: false,
            dirty: false,
            staged_files: Vec::new(),
            unstaged_files: Vec::new(),
            untracked_files: Vec::new(),
            conflicted_files: Vec::new(),
            conflict_entries: Vec::new(),
            branch_files: Vec::new(),
            branch_diff_coverage: crate::coverage::Coverage::unrecorded(),
            recent_commits: Vec::new(),
        }
    }

    fn quiet_consumption() -> Consumption {
        Consumption {
            intent: "test".to_string(),
            intent_source: "declared at launch",
            expansions: 0,
            handoffs: 0,
        }
    }

    #[test]
    fn summary_flags_a_moved_head_and_branch_switch() {
        let entry = state("main", "aaaaaaaaaa");
        let exit = state("feature", "bbbbbbbbbb");
        let out = render_run_summary(&entry, &exit, 3, &quiet_consumption());
        assert!(out.contains("branch:    main → feature"), "{out}");
        assert!(
            out.contains("HEAD:      aaaaaaa → bbbbbbb (moved)"),
            "{out}"
        );
        assert!(out.contains("refreshed: 3 time(s)"), "{out}");
    }

    #[test]
    fn summary_omits_head_line_when_head_is_unchanged() {
        let unchanged = state("main", "abc1234");
        let out = render_run_summary(&unchanged, &unchanged, 0, &quiet_consumption());
        // No commits landed → no HEAD line, and no spurious "→" on the branch.
        assert!(!out.contains("HEAD:"), "{out}");
        assert!(out.contains("branch:    main\n"), "{out}");
        assert!(
            out.contains("worktree:  0 staged, 0 unstaged, 0 untracked"),
            "{out}"
        );
    }

    #[test]
    fn summary_reports_consumption_honestly_in_both_directions() {
        let unchanged = state("main", "abc1234");
        let quiet = render_run_summary(&unchanged, &unchanged, 0, &quiet_consumption());
        assert!(
            quiet.contains("intent:    \"test\" (declared at launch)"),
            "{quiet}"
        );
        assert!(quiet.contains("expansions: none declared"), "{quiet}");
        assert!(quiet.contains("handoff:   none recorded"), "{quiet}");

        let busy = Consumption {
            intent: "widened".to_string(),
            intent_source: "updated mid-session",
            expansions: 2,
            handoffs: 1,
        };
        let out = render_run_summary(&unchanged, &unchanged, 0, &busy);
        assert!(
            out.contains("intent:    \"widened\" (updated mid-session)"),
            "{out}"
        );
        assert!(
            out.contains("expansions: 2 scope path(s) declared while working"),
            "{out}"
        );
        assert!(out.contains("handoff:   1 record(s)"), "{out}");
    }

    #[test]
    fn refusal_suggestion_is_copy_pasteable() {
        let refusal =
            refuse_missing_intent(&["sh".to_string(), "-c".to_string(), "echo hi".to_string()]);
        assert_eq!(refusal.reason, "missing-intent");
        // Multi-word arguments come back shell-quoted so the suggestion parses.
        assert!(
            refusal
                .message
                .contains("toimija run --intent \"<why>\" -- sh -c 'echo hi'"),
            "{}",
            refusal.message
        );
        assert!(refusal.message.contains("--intent unknown"));
    }

    #[test]
    fn shell_word_quotes_only_when_needed() {
        assert_eq!(shell_word("codex"), "codex");
        assert_eq!(shell_word("./bin/agent-2"), "./bin/agent-2");
        assert_eq!(shell_word("echo hi"), "'echo hi'");
        assert_eq!(shell_word("it's"), r"'it'\''s'");
        assert_eq!(shell_word(""), "''");
    }

    #[test]
    fn short_oid_is_safe_for_short_input() {
        assert_eq!(short_oid("abc"), "abc");
        assert_eq!(short_oid(""), "");
        assert_eq!(short_oid("0123456789"), "0123456");
    }
}
