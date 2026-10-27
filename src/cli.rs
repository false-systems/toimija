use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use tracing::{info, info_span, warn};

use crate::command::{CommandRunner, ProcessCommandRunner};
use crate::config::{self, RepoConfig};
use crate::consistency::{CONSISTENCY_ALGORITHM, Consistency, GitOperationState, Verdict};
use crate::context;
use crate::footprint::{self, HistoryEnd};
use crate::git;
use crate::landed;
use crate::logging::LogFormat;
use crate::refresh;
use crate::render;
use crate::repo;
use crate::session;
use crate::sessions;
use crate::snapshot;
use crate::validate;
use crate::watch;

/// `toimija --version`: the version number and the commit the binary was
/// built from (`build.rs`), so two builds of one version can be told apart.
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("TOIMIJA_BUILD_COMMIT"),
    ")"
);

#[derive(Debug, Parser)]
#[command(
    name = "toimija",
    version = VERSION,
    about = "Git-aware local context compiler for coding agents",
    long_about = "Git-aware local context compiler for coding agents.\n\n\
        Toimija starts from Git's state machine — branch, HEAD, index, merge-base, \
        staged/unstaged/untracked files — and compiles it into a compact work packet \
        an agent reads before touching the repo, instead of crawling it cold.",
    after_help = "Typical agent workflow:\n  \
        toimija enter --for-diff --budget 4000    # preflight: refresh + print the work packet\n  \
        toimija run --intent \"fix #51\" -- codex   # wrap an agent in a live session packet\n  \
        toimija sessions                          # who else is working in this repo right now\n  \
        toimija validate --plan                   # print the advisory validation plan\n  \
        toimija -q repository-point --json        # the exact repository point, for tools\n\n\
        Boundaries: Toimija reports facts and refuses to decide. It is local-only: \
        it writes only under .toimija/ and, for snapshots, Git objects and \
        refs/toimija/; it never executes validation or gates, starts no daemons, \
        and makes no network calls of its own."
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
    /// tell your agents there to run `toimija enter` first.
    ///
    /// Use this once per repo to seed the config. It never overwrites an
    /// existing file. Writes only .toimija/repo.yaml and .toimija/.gitignore.
    Init,
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
    /// merge-base, changed surface, what to read first, and the validation
    /// plan. `enter` prints the same packet and also writes
    /// `.toimija/current.*` — prefer it as the agent preflight.
    /// Read-only; no validation execution, no network.
    #[command(
        after_help = "Examples:\n  toimija context --for-diff --budget 4000   # diff-focused, capped for a prompt\n  \
        toimija context --json                     # machine-readable packet\n  \
        toimija context --identity --json          # only what the repo says it is"
    )]
    Context(ContextCommandArgs),
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
    /// Read first list tell you what to read first. Writes only the two packet
    /// files; no validation execution, no network.
    #[command(
        after_help = "Examples:\n  toimija enter --for-diff --budget 4000   # the standard agent preflight\n  \
        toimija enter --json                     # machine-readable entry packet"
    )]
    Enter(ContextArgs),
    /// [experimental] Watch the repo and refresh .toimija/current.* on change (foreground).
    ///
    /// Use this for a human-driven session where an editor open in the repo
    /// should keep the packet current. Debounced; ignores build dirs and its
    /// own writes. No daemon, no network, no validation execution — runs only
    /// in the foreground until stopped (Ctrl-C). `run` bundles this same watch
    /// around a wrapped agent command.
    Watch(ContextArgs),
    /// [experimental] Enter, watch while an agent command runs, then exit with its status.
    ///
    /// The flagship wrapper — use this to wrap any agent command in a live
    /// session: it opens a per-session packet directory under
    /// `.toimija/sessions/<id>/`, prints the entry packet, keeps it fresh while
    /// the command runs, mirrors `.toimija/current.*`, and exits with the
    /// wrapped command's status (128+signal preserved). `--intent` is required
    /// (`--intent unknown` remains the explicit escape hatch). Known agents (claude, codex, gemini)
    /// get the packet inlined in their launch prompt — pass the task via
    /// `--task`; other commands keep the TOIMIJA_* env handshake behind an
    /// honest banner. `--scope` tells sibling sessions what you expect to touch
    /// — advisory, never an assignment.
    ///
    /// Outside any Git repository it opens a session with no packet instead:
    /// the marker lives in the machine-local state directory
    /// (`$TOIMIJA_STATE_DIR`, else `$XDG_STATE_HOME/toimija`, else
    /// `~/.local/state/toimija`), never in the folder, and
    /// `toimija sessions --outside-repos` lists it.
    #[command(
        after_help = "Examples:\n  toimija run --intent \"fix #51\" --task \"fix the login redirect\" -- claude   # packet + task in the launch prompt\n  \
        toimija run --intent \"fix #51\" --scope src/cli.rs -- codex   # declare intent/scope for siblings\n  \
        toimija run --intent unknown --for-diff --budget 4000 -- sh -c '…'   # tune the packet the child sees\n  \
        cd ~/projects && toimija run --intent \"cross-repo sweep\" --scope ~/projects/a -- claude   # outside a repo: a session, no packet"
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
    /// [experimental] List sibling sessions live in this repo: who is here, on which branch and
    /// worktree, with what intent and scope, and where overlap warrants care.
    ///
    /// Use this before starting work in a shared repo: it reports presence
    /// (LIVE / STALE / CLOSED) and descriptive overlap warnings. Read-only
    /// unless `--prune`, which runs the same configured stewardship pass as
    /// `clean` (including expired sessions). Assigns nothing, signals nothing,
    /// no network. Outside a repository, or with `--outside-repos`, it lists
    /// the sessions `run` opened outside any repository instead.
    #[command(
        after_help = "Examples:\n  toimija sessions           # LIVE / STALE / CLOSED listing + warnings\n  \
        toimija sessions --json    # machine-readable report\n  \
        toimija sessions --prune   # run the configured stewardship pass\n  \
        toimija sessions --outside-repos   # sessions opened outside any repository"
    )]
    Sessions(SessionsArgs),
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
    /// selection. Use this before every packet read; it exits non-zero (naming
    /// the reason) when the packet is stale or briefs a session that has ended,
    /// so a caller refreshes before trusting it. Read-only; no network.
    #[command(
        after_help = "Examples:\n  toimija verify --json || toimija refresh   # verify the ambient packet\n  \
        toimija verify --packet .toimija/current.json --json  # select explicitly"
    )]
    Verify(VerifyArgs),
    /// Export the exact current repository point using false-agent-protocol v1.
    ///
    /// Materializes the worktree tree and pairs it with Toimija's consistency
    /// token. Toimija remains authoritative for this point.
    #[command(after_help = "Examples:\n  toimija repository-point --json")]
    RepositoryPoint(SemanticArgs),
    /// [experimental] Materialize the working tree as an immutable git tree object.
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
pub struct ContextCommandArgs {
    #[command(flatten)]
    pub packet: ContextArgs,
    /// Emit only the packet's `repo` object — what `.toimija/repo.yaml` says
    /// the repository is — read without collecting Git state. Requires `--json`.
    #[arg(long, requires = "json", conflicts_with_all = ["for_diff", "budget"])]
    pub identity: bool,
}

#[derive(Debug, Args)]
pub struct SemanticArgs {
    /// Emit the versioned machine projection as JSON.
    #[arg(long)]
    pub json: bool,
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
    /// known agent (`claude`, `codex`, `gemini`) or one configured under
    /// `agents:` in `.toimija/repo.yaml`. Pass the task here, not as the agent's
    /// own positional argument. May start with `-`; text that is one of `run`'s
    /// own flags is written `--task=<text>`. A blank task is none.
    #[arg(long, allow_hyphen_values = true)]
    pub task: Option<String>,
    /// Background instructions, separate from an assigned task: appended after
    /// the packet (and the task) in a known or configured agent's launch
    /// prompt. An idle Codex launch receives them with its packet as session
    /// instructions, without starting a model turn. An agent toimija does not
    /// recognise cannot receive them. May start with `-`; text that is one of
    /// `run`'s own flags is written `--briefing=<text>`. A blank briefing is
    /// none.
    #[arg(long, allow_hyphen_values = true)]
    pub briefing: Option<String>,
    /// An urgent line for the agent, the first thing in its launch prompt,
    /// above the contract and the packet (repeatable). For a launcher's flags
    /// and warnings: at most 1024 bytes in total, the rest is cut. Long
    /// background goes in `--briefing`. May start with `-`; text that is one
    /// of `run`'s own flags is written `--alert=<text>`. A blank alert is none.
    #[arg(long = "alert", value_name = "TEXT", allow_hyphen_values = true)]
    pub alert: Vec<String>,
    /// The agent command to run, after `--` (e.g. `-- claude ...`).
    #[arg(trailing_var_arg = true, required = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

/// The options of `run` whose text may start with `-` (a briefing is
/// Markdown, whose lists do), so clap gives each the next argument whatever
/// that is.
const RUN_TEXT_OPTIONS: [&str; 3] = ["--task", "--briefing", "--alert"];

/// Issue #237: `--briefing --task "fix it"` took `--task` as the briefing and
/// launched a program named `fix it`. When the argument after a spaced
/// `--task`, `--briefing` or `--alert` is one of `run`'s own flags, or the
/// `--` before the command, the value was left out, and that is refused as a
/// usage error before anything runs. The `=` form says the value is intended,
/// so `--task=--briefing` passes the text `--briefing`. The flags are read from
/// the command itself, globals included, so a flag added to `run` later is
/// covered without being listed here. Read from the raw arguments, because
/// clap gives a value parser the same text for both forms.
fn missing_run_text(args: &[std::ffi::OsString]) -> Option<clap::Error> {
    use clap::CommandFactory;
    let mut top = Cli::command();
    top.build();
    let mut rest = args.iter().skip(1).map(|arg| arg.to_str());
    // Past the global options to the subcommand.
    loop {
        match rest.next()? {
            Some(token) if token.starts_with('-') && token != "--" => {
                if takes_a_separate_value(&top, token) {
                    rest.next();
                }
            }
            Some("run") => break,
            _ => return None,
        }
    }
    let run = top.find_subcommand("run")?;
    while let Some(token) = rest.next() {
        match token {
            // The command begins: what follows is the agent's.
            None | Some("--") => return None,
            Some(token) if !token.starts_with('-') => return None,
            Some(token) if RUN_TEXT_OPTIONS.contains(&token) => match rest.next() {
                Some(Some(next)) if names_an_option(run, next) => {
                    let given = if next == "--" {
                        "the `--` before the command".to_string()
                    } else {
                        format!("`{next}`, one of `run`'s own flags,")
                    };
                    let message = format!(
                        "`{token}` is missing its value: {given} is not text. Give `{token}` \
                         its text, or write `{token}=<text>` for text that looks like a flag.\n"
                    );
                    return Some(
                        clap::Error::raw(clap::error::ErrorKind::InvalidValue, message)
                            .with_cmd(run),
                    );
                }
                Some(_) => {}
                None => return None,
            },
            Some(token) => {
                if takes_a_separate_value(run, token) {
                    rest.next();
                }
            }
        }
    }
    None
}

/// Whether `token`, an option of `cmd` written without `=`, takes the next
/// argument as its value (`--intent <why>`, `--log-format <format>`).
fn takes_a_separate_value(cmd: &clap::Command, token: &str) -> bool {
    let takes = |arg: &clap::Arg| arg.get_action().takes_values();
    if let Some(long) = token.strip_prefix("--") {
        return !long.contains('=')
            && cmd
                .get_arguments()
                .any(|arg| arg.get_long() == Some(long) && takes(arg));
    }
    let mut chars = token.chars().skip(1);
    match (chars.next(), chars.next()) {
        (Some(short), None) => cmd
            .get_arguments()
            .any(|arg| arg.get_short() == Some(short) && takes(arg)),
        _ => false,
    }
}

/// Whether `text` is `--`, or one of `cmd`'s flags as a user would spell it:
/// `--name`, `--name=value`, or `-x`.
fn names_an_option(cmd: &clap::Command, text: &str) -> bool {
    if text == "--" {
        return true;
    }
    if let Some(long) = text.strip_prefix("--") {
        let name = long.split_once('=').map_or(long, |(name, _)| name);
        return cmd.get_arguments().any(|arg| {
            arg.get_long() == Some(name)
                || arg
                    .get_all_aliases()
                    .is_some_and(|aliases| aliases.contains(&name))
        });
    }
    let mut chars = text.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some('-'), Some(short), None) => cmd
            .get_arguments()
            .any(|arg| arg.get_short() == Some(short)),
        _ => false,
    }
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
    /// List the sessions `run` opened outside any Git repository (kept in the
    /// machine-local state directory) instead of this repository's. The
    /// default when not inside a repository.
    #[arg(long)]
    pub outside_repos: bool,
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Emit the verdict as JSON: `{ "status": "verified", "packet_path", "token",
    /// "artifact_digest", ... }`, or on failure a non-`verified` status
    /// (`stale_packet`, `session_not_live`, `missing_packet`, `invalid_packet`,
    /// `seal_digest_mismatch`, `unsafe_git_operation`, ...) with its details.
    /// The exit code is 0 only for `verified`.
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

impl Cli {
    pub fn parse_args() -> Self {
        let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
        if let Some(refusal) = missing_run_text(&args) {
            refusal.exit();
        }
        Self::parse_from(args)
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
        Command::Context(ContextCommandArgs { packet: args, .. })
        | Command::Enter(args)
        | Command::Refresh(args)
            if args.json =>
        {
            "json"
        }
        Command::History(args) if args.json => "json",
        Command::Landed(args) if args.json => "json",
        Command::RepositoryPoint(args) if args.json => "json",
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
        Command::Doctor(args) => run_doctor(args).map(|()| 0),
        Command::Context(args) => run_context(args).map(|()| 0),
        Command::Refresh(args) => run_refresh(args).map(|()| 0),
        Command::Enter(args) => run_enter(args).map(|()| 0),
        Command::Watch(args) => run_watch(args).map(|()| 0),
        Command::Run(args) => run_run(args),
        Command::Intent(args) => run_intent(args).map(|()| 0),
        Command::History(args) => run_history(args).map(|()| 0),
        Command::Clean(args) => run_clean(args).map(|()| 0),
        Command::Landed(args) => run_landed(args).map(|()| 0),
        Command::Sessions(args) => run_sessions(args).map(|()| 0),
        Command::Validate(args) => run_validate(args).map(|()| 0),
        Command::Verify(args) => run_verify(args),
        Command::RepositoryPoint(args) => run_repository_point(args).map(|()| 0),
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
            Command::Doctor(_) => "doctor",
            Command::Context(_) => "context",
            Command::Refresh(_) => "refresh",
            Command::Enter(_) => "enter",
            Command::Watch(_) => "watch",
            Command::Run(_) => "run",
            Command::Intent(_) => "intent",
            Command::History(_) => "history",
            Command::Clean(_) => "clean",
            Command::Landed(_) => "landed",
            Command::Sessions(_) => "sessions",
            Command::Validate(_) => "validate",
            Command::Verify(_) => "verify",
            Command::RepositoryPoint(_) => "repository-point",
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

fn run_context(args: ContextCommandArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    if args.identity {
        let repo_root = git::discover_repo_root(&runner, &cwd)
            .map_err(anyhow::Error::from)
            .context("failed to build context packet")?;
        let identity = context::identity(&repo_root).context("failed to build context packet")?;
        println!("{}", serde_json::to_string_pretty(&identity)?);
        return Ok(());
    }
    let args = args.packet;
    let mut packet = context::build_context_packet(&runner, &cwd, args.for_diff)
        .context("failed to build context packet")?;
    packet.attach_seal(None)?;
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

fn run_repository_point(args: SemanticArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let worktree = git::discover(&runner, &cwd)?;
    // The point is the token and the tree, and nothing else a packet holds:
    // only what the token fingerprints is read. The configuration is still
    // loaded, as compiling the packet did, so a broken repo.yaml fails the
    // point as it always has.
    RepoConfig::load_optional(&worktree.root)?;
    // Each attempt reads the repository afresh: a point whose worktree moved
    // under the tree write is retried, and refused once it keeps moving: a move
    // the token observes is never printed as a tree from after it beside a token
    // from before it.
    let point = crate::semantic::retry_moved(|| {
        let mut git = git::token_state(&runner, &worktree)?;
        context::normalize_git_state(&mut git);
        let consistency = Consistency::fingerprint(&git);
        crate::semantic::repository_point(&runner, &worktree, &git, &consistency)
    })?;
    let point = crate::semantic::point_envelope(point);
    point.validate().map_err(anyhow::Error::from)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&point)?);
    } else {
        println!("repository point: {}", point.repository.id);
        println!("tree: {}", point.repository.tree_oid.0);
        println!("consistency: {}", point.repository.consistency_token.0);
        println!(
            "branch: {}",
            point
                .repository
                .branch
                .as_ref()
                .map(|branch| branch.0.as_str())
                .unwrap_or("detached")
        );
    }
    info!(
        event = "repository_point_refreshed",
        repository = %point.repository.repository,
        repository_point = %point.repository.id,
        protocol_major = point.protocol.major,
        protocol_minor = point.protocol.minor
    );
    Ok(())
}

fn run_refresh(args: ContextArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let worktree = git::discover(&runner, &cwd).context("not a Git repository")?;
    let repo_root = &worktree.root;
    let refreshed = ambient_refresh(&runner, &worktree, &args)?;
    if args.json {
        println!("{}", refreshed.json);
    } else {
        println!(
            "Refreshed {} and {}",
            rel_display(&refreshed.md_path, repo_root),
            rel_display(&refreshed.json_path, repo_root)
        );
    }
    Ok(())
}

fn run_enter(args: ContextArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let worktree = git::discover(&runner, &cwd).context("not a Git repository")?;
    let repo_root = &worktree.root;
    // Enter is refresh + print: it writes the live packet, then frames it as the
    // work preflight an agent reads on entering the repo.
    let refreshed = ambient_refresh(&runner, &worktree, &args)?;
    if args.json {
        println!("{}", refreshed.json);
    } else {
        print_agent_entry(&refreshed, repo_root);
    }
    opportunistic_sweep(&runner, repo_root);
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
    worktree: &git::Worktree,
    args: &ContextArgs,
) -> Result<refresh::Refreshed> {
    let repo_root = &worktree.root;
    let ambient = session::ambient_session(repo_root);
    let plan = refresh::Plan {
        for_diff: args.for_diff,
        budget: args.budget,
        session_dir: ambient.as_ref().map(|(_, dir)| dir.clone()),
    };
    let refreshed = plan.run(runner, worktree)?;
    if let Some((id, _)) = &ambient
        && let Err(error) = session::touch(repo_root, id)
    {
        warn!(event = "toimija.session.heartbeat_failed", error = %format!("{error:#}"));
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

fn run_run(mut args: RunArgs) -> Result<i32> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    args.task = args.task.filter(|task| !task.trim().is_empty());
    args.briefing = args.briefing.filter(|briefing| !briefing.trim().is_empty());
    args.alert.retain(|alert| !alert.trim().is_empty());

    // Armed before a session can exist, not after. Arming once the marker is
    // on disk leaves a window — `Session::open` returning — where a signal
    // still kills the wrapper with the marker `active`. That window is
    // microseconds in an ordinary run and wide enough to hit reliably under
    // coverage instrumentation, which is how it was found. A signal arriving
    // before this point has no marker to strand.
    let termination =
        TerminationWatch::install().context("failed to register termination signal handlers")?;

    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let worktree = match git::discover(&runner, &cwd) {
        Ok(worktree) => worktree,
        // Git ran and said no: a session outside any repository (issue #225).
        // A missing git stays an error — that is not the same as "no repo".
        Err(crate::error::ToimijaError::NotGitRepo) => {
            return run_outside_repo(args, &cwd, &termination);
        }
        Err(error) => return Err(error).context("not a Git repository"),
    };
    let repo_root = worktree.root.clone();
    if args.intent.is_none() {
        return Err(refuse_missing_intent(&args.command).into());
    }
    // Asked to stop before any marker exists: leave without creating one.
    if let Some(number) = termination.requested() {
        eprintln!("toimija run: termination requested before the session opened");
        return Ok(i32::try_from(128 + number).unwrap_or(1));
    }

    // 0. Stamp the Git files of the discovered worktree, for the watcher,
    //    before any packet is built from them, so the watcher's first check
    //    compares against state from before the entry packet.
    let watch_repo = watch::Repository::of(&runner, &worktree)
        .context("failed to start the file watcher for `run` (try `toimija enter` then run the command directly)")?;

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
    let entry = match plan.run(&runner, &worktree) {
        Ok(entry) => entry,
        Err(error) => {
            close_session_after_startup_failure(&repo_root, &mut session);
            return Err(error);
        }
    };
    let refs_held = watch_repo.refs_held();
    let entry = match verify_run_entry(&runner, &worktree, &plan, entry, refs_held, &session.id) {
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
    let mut watch_session = match watch::start(watch_repo)
        .context("failed to start the file watcher for `run` (try `toimija enter` then run the command directly)")
    {
        Ok(watch_session) => watch_session,
        Err(error) => {
            close_session_after_startup_failure(&repo_root, &mut session);
            return Err(error);
        }
    };
    // `catch_up` refreshes only when something moved between the entry packet
    // and the watcher's registration. Launch with the packet as it stands now:
    // the entry packet, or the caught-up one, verified.
    let entry = match watch_session
        .catch_up(&runner, &plan, &entry)
        .context("failed to refresh after arming the file watcher")
        .and_then(|caught_up| match caught_up {
            None => Ok(entry),
            Some(refreshed) => {
                let refs_held = watch_session.refs_held();
                verify_run_entry(&runner, &worktree, &plan, refreshed, refs_held, &session.id)
            }
        }) {
        Ok(entry) => entry,
        Err(error) => {
            close_session_after_startup_failure(&repo_root, &mut session);
            return Err(error);
        }
    };
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
        watch_session.run(&runner, &watch_plan, &watcher_stop, || {
            watcher_refreshes.fetch_add(1, Ordering::Relaxed);
            // Heartbeat (proposal 0002): an event worth a refresh is worth a
            // `last_seen_at` stamp, so staleness measures inactivity rather
            // than lifetime. Best-effort — never interrupts the watch loop.
            if let Err(error) = session::touch(&watch_root, &heartbeat_id) {
                warn!(event = "toimija.session.heartbeat_failed", error = %format!("{error:#}"));
            }
        })
    });

    // Interrupted during entry or watcher arming: close the marker and leave,
    // rather than launching an agent the operator has already asked to stop.
    if let Some(number) = termination.requested() {
        stop.store(true, Ordering::Relaxed);
        let _ = watcher.join();
        close_session_after_startup_failure(&repo_root, &mut session);
        eprintln!("toimija run: termination requested before the agent launched — session closed");
        return Ok(i32::try_from(128 + number).unwrap_or(1));
    }

    // 5. Launch the agent command, inheriting the terminal. The child also
    //    inherits the per-session handshake (`TOIMIJA_RUN`, `TOIMIJA_SESSION`,
    //    `TOIMIJA_LIVE*`, `TOIMIJA_REPO_LIVE`), so a wrapped agent can *discover*
    //    its own continuously-refreshed packet instead of hardcoding a path.
    let (program, rest) = args
        .command
        .split_first()
        .expect("clap `required = true` guarantees at least one argument");
    // Proposal 0006 Part C: for a known or configured agent the packet travels
    // in argv — in context by construction. Anything else keeps the env-var
    // handshake, and says so instead of silently degrading. The `agents:`
    // section of repo.yaml extends the built-in table so any harness can be
    // taught delivery without patching this crate.
    let repo_config = RepoConfig::load_optional(&repo_root)?
        .unwrap_or_else(|| RepoConfig::default_for(&repo_root));
    let live_packet = session.dir.join("current.md");
    let mut composed = crate::adapters::agents::compose_launch_prompt(
        program,
        &entry.markdown,
        &live_packet,
        args.task.as_deref(),
        &repo_config.agents,
    );
    finish_prompt(&mut composed, &args);
    match &composed {
        Some(_)
            if crate::adapters::agents::idle_codex(
                program,
                rest,
                args.task.is_some(),
                &repo_config.agents,
            ) =>
        {
            eprintln!(
                "toimija run: delivery: Codex session instructions; waiting for your first task"
            );
        }
        Some(_) => {
            eprintln!(
                "toimija run: delivery: launch prompt with the packet inlined for `{program}`"
            );
            say_own_codex_instructions_kept(program, rest, &args, &repo_config.agents);
        }
        None => {
            eprintln!("toimija run: delivery: env-var only — not guaranteed for `{program}`");
            let items = undelivered(true, &args);
            warn_undelivered(
                program,
                "is not a known or configured agent",
                &items,
                "or teach toimija its prompt under `agents:` in .toimija/repo.yaml",
            );
            record_undelivered(&mut session, items);
        }
    }
    // The prompt travels where the agent reads it: last positional argument,
    // or after the flag its `agents:` entry names.
    let handshake = session.env(&repo_root);
    let child_args =
        match crate::adapters::agents::shell_env_args(program, &handshake, session::RETIRED_ENV)
            .and_then(|mut child_args| {
                child_args.extend_from_slice(rest);
                if let Some(prompt) = composed {
                    child_args.extend(crate::adapters::agents::launch_args(
                        program,
                        rest,
                        prompt,
                        args.task.is_some(),
                        &repo_config.agents,
                    )?);
                }
                Ok(child_args)
            }) {
            Ok(args) => args,
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                let _ = watcher.join();
                close_session_after_startup_failure(&repo_root, &mut session);
                return Err(error);
            }
        };
    eprintln!(
        "toimija run: launching `{program}` (session {}, watching for changes)",
        session.id
    );
    let mut child = std::process::Command::new(program);
    child
        .args(&child_args)
        .current_dir(&repo_root)
        .envs(handshake);
    for retired in session::RETIRED_ENV {
        child.env_remove(retired);
    }
    // From here the terminal belongs to the wrapped command. Seal this
    // process's own stdout/stderr at the descriptor level onto the session
    // diagnostics log — watcher output, refresh warnings, even panic messages
    // land there — and hand the saved real terminal to the child. Nothing we
    // print can interleave with the harness while it runs.
    let diagnostics_path = session.dir.join("diagnostics.log");
    let capture = match crate::diagnostics::Capture::borrow(&diagnostics_path) {
        Ok(capture) => Some(capture),
        Err(error) => {
            // Best-effort isolation: say so before the launch, while the
            // terminal is still ours, rather than fail the run over a log.
            eprintln!(
                "toimija run: could not open the diagnostics log ({}); background output may interleave with the command: {error:#}",
                rel_display(&diagnostics_path, &repo_root)
            );
            None
        }
    };
    if let Some(capture) = &capture {
        child.stdout(
            capture
                .child_stdout()
                .context("failed to hand the wrapped command our stdout")?,
        );
        child.stderr(
            capture
                .child_stderr()
                .context("failed to hand the wrapped command our stderr")?,
        );
    }
    let launched = supervise_agent(&mut child, &termination);

    // 6. Stop the watcher and join, whatever happened to the command. A loop
    //    error (e.g. a mid-run refresh failure) is reported, but the wrapped
    //    command's status still wins the exit code — the child has already run.
    stop.store(true, Ordering::Relaxed);
    let watcher_result = watcher.join();
    // The wrapped command is gone and the watcher with it: take the terminal
    // back. The capture count is final once the watcher is joined.
    let captured = capture.map_or(0, crate::diagnostics::Capture::finish);
    // What the watch knew when it stopped: the exit refresh carries it.
    let stopped = match watcher_result {
        Ok(Ok(stopped)) => Some(stopped),
        Ok(Err(error)) => {
            eprintln!("toimija run: watcher stopped on error: {error:#}");
            None
        }
        // The default panic hook already printed message+location — into the
        // capture log, if it happened while the terminal was sealed; still,
        // include the payload here so the join site is not silent about *what*
        // panicked.
        Err(payload) => {
            eprintln!(
                "toimija run: watcher thread panicked: {}",
                panic_message(payload.as_ref())
            );
            None
        }
    };
    if captured > 0 {
        eprintln!(
            "toimija run: captured {captured} diagnostic line(s) while the wrapped command ran — {}",
            rel_display(&diagnostics_path, &repo_root)
        );
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
        let consumption = collect_consumption(&session, &args);
        // One `git status`, carrying the last packet's ref-derived facts while
        // the refs are where the watch last read them; otherwise read afresh.
        let exit = match &stopped {
            Some(stopped) => plan.carrying(&runner, stopped.worktree(), stopped.holding()),
            None => plan.run(&runner, &worktree.lasting()),
        };
        match exit {
            Ok(exit) => {
                eprint!(
                    "\n{}",
                    render_run_summary(
                        &entry.packet.git,
                        &exit.packet.git,
                        refreshes.load(Ordering::Relaxed),
                        &consumption,
                    )
                );
            }
            Err(error) => eprintln!("toimija run: could not compute exit summary: {error:#}"),
        }
    }
    remove_session_point(&repo_root, &session);

    // 8. Exit with the wrapped command's status — preserving signal semantics so
    //    a cancelled/terminated child is distinguishable from a generic failure.
    //    Returned (not `process::exit`) so the command.finish bookend still runs.
    let status = launched.with_context(|| format!("failed to launch `{program}`"))?;
    Ok(wrapped_exit_code(status))
}

/// Termination signals set a flag instead of killing the wrapper, for as long
/// as this value lives.
///
/// Registration has to begin the moment a session marker exists on disk, not
/// when the agent launches. A signal arriving during entry, watcher arming, or
/// the first refresh would otherwise kill toimija outright with the marker
/// still `status: active`, where it sits until the 7-day retention sweeps it —
/// every interrupted run replenishing the stale-session warnings.
///
/// The flag records *which* signal arrived, so the exit code can stay honest at
/// 128 + signal rather than guessing one.
struct TerminationWatch {
    signal: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(unix)]
    registrations: Vec<signal_hook::SigId>,
}

impl TerminationWatch {
    #[cfg(unix)]
    fn install() -> std::io::Result<Self> {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        let signal = Arc::new(AtomicUsize::new(0));
        let registrations = [
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGHUP,
            signal_hook::consts::SIGINT,
        ]
        .into_iter()
        .map(|number| {
            signal_hook::flag::register_usize(
                number,
                Arc::clone(&signal),
                usize::try_from(number).unwrap_or(0),
            )
        })
        .collect::<std::io::Result<Vec<_>>>()?;
        Ok(Self {
            signal,
            registrations,
        })
    }

    #[cfg(not(unix))]
    fn install() -> std::io::Result<Self> {
        Ok(Self {
            signal: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    /// The signal that arrived, if one has, leaving the flag set.
    fn requested(&self) -> Option<usize> {
        match self.signal.load(std::sync::atomic::Ordering::SeqCst) {
            0 => None,
            number => Some(number),
        }
    }

    /// The signal that arrived since the last call, clearing the flag so a
    /// second signal is distinguishable from the first.
    fn take(&self) -> Option<usize> {
        match self.signal.swap(0, std::sync::atomic::Ordering::SeqCst) {
            0 => None,
            number => Some(number),
        }
    }
}

impl Drop for TerminationWatch {
    fn drop(&mut self) {
        #[cfg(unix)]
        for id in self.registrations.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

/// Run the wrapped agent to completion, forwarding termination signals to it.
///
/// `Command::status()` alone would orphan the agent: a signal delivered to this
/// process only (`kill <toimija-pid>`, a closing terminal's SIGHUP) kills the
/// wrapper, and the agent tree is reparented to init and keeps running headless.
/// The agent intentionally stays in this process's process group — it is an
/// interactive foreground program and must keep the terminal (a background
/// process group reading the tty is stopped with SIGTTIN) — so Ctrl-C still
/// reaches it directly from the tty; forwarded here are the signals that target
/// this PID specifically. Registering SIGINT also means an interrupt no longer
/// kills the wrapper before it can close the session marker and print the exit
/// summary.
#[cfg(unix)]
fn supervise_agent(
    command: &mut std::process::Command,
    termination: &TerminationWatch,
) -> std::io::Result<std::process::ExitStatus> {
    let mut child = command.spawn()?;
    let mut forwarded_at: Option<Instant> = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        if termination.take().is_some() && forwarded_at.is_none() {
            if let Ok(pid) = i32::try_from(child.id()) {
                // SAFETY: pid is the live child just spawned above; SIGTERM to
                // a single PID we own is the intended forwarding.
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
                eprintln!(
                    "toimija run: termination requested — forwarded to the agent (pid {pid})"
                );
            }
            forwarded_at = Some(Instant::now());
        }
        if let Some(at) = forwarded_at
            && should_force_kill(at.elapsed())
        {
            let _ = child.kill();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    Ok(status)
}

#[cfg(not(unix))]
fn supervise_agent(
    command: &mut std::process::Command,
    _termination: &TerminationWatch,
) -> std::io::Result<std::process::ExitStatus> {
    command.status()
}

/// Escalation policy after SIGTERM has been forwarded to the agent.
///
/// Called once per 50ms poll tick; `since_forward` is how long ago the SIGTERM
/// went out. Return true to SIGKILL the agent now.
///
/// Policy: a fixed grace window. An agent mid-write (session transcript, git
/// index) gets time to flush and exit on the SIGTERM; a hung agent — the
/// battery-drain incident this function exists to end — is force-killed once
/// the window closes. Interactive agents normally exit on SIGTERM well under
/// ten seconds, so twenty is generous without being unbounded.
#[cfg(unix)]
fn should_force_kill(since_forward: std::time::Duration) -> bool {
    const GRACE: std::time::Duration = std::time::Duration::from_secs(20);
    since_forward > GRACE
}

/// The gate before launch: the entry packet must still match the live
/// repository, or it is refreshed once and checked again, and then refused.
///
/// The live state is read with one `git status`. While `refs_held` — the refs
/// are where they were before the entry packet's Git state was read — the
/// packet's own ref-derived facts are carried beside it ([`git::restatus`]),
/// so the refresh token compares the state a full read would find. Otherwise,
/// and after a refresh here, everything is read again.
fn verify_run_entry(
    runner: &impl crate::command::CommandRunner,
    worktree: &git::Worktree,
    plan: &refresh::Plan,
    mut entry: refresh::Refreshed,
    refs_held: bool,
    session_id: &str,
) -> Result<refresh::Refreshed> {
    let mut refs_held = refs_held;
    for attempt in 0..=1 {
        let live = if refs_held {
            git::restatus(runner, worktree, &entry.collected)
        } else {
            git::collect(runner, worktree)
        }
        .context("failed to verify the `run` entry packet")?;
        let mut live = live.state;
        context::normalize_git_state(&mut live);
        refs_held = false;
        match entry.packet.consistency.verify(&live) {
            Verdict::Consistent => {
                let refresh_changed = entry
                    .packet
                    .refresh_token
                    .as_ref()
                    .is_some_and(|expected| context::refresh_token(&live) != *expected);
                if refresh_changed && attempt == 0 {
                    warn!(
                        event = "toimija.run.packet_state_raced",
                        session = %session_id,
                        "refreshing once before launch"
                    );
                    entry = plan
                        .run(runner, worktree)
                        .context("failed to refresh the raced `run` entry packet")?;
                    continue;
                }
                anyhow::ensure!(
                    !refresh_changed,
                    "refusing to launch: packet-visible repository state changed again after one refresh"
                );
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
                    .run(runner, worktree)
                    .context("failed to refresh the raced `run` entry packet")?;
            }
            Verdict::Stale { .. } => anyhow::bail!(
                "refusing to launch: the `run` entry packet changed again after one refresh"
            ),
            Verdict::NotComparable(algorithm) => anyhow::bail!(
                "refusing to launch: the entry packet's token is `{algorithm}`, not comparable \
                 with this toimija's `{CONSISTENCY_ALGORITHM}`"
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

/// `run` outside any Git repository (issue #225): a visible session with no
/// packet. The marker lives under the machine-local state root (never in
/// `folder`), a heartbeat keeps it from reading stale while the agent works,
/// and `toimija sessions --outside-repos` lists it. What needs Git — the
/// packet and its watcher, the exit summary — is not
/// attempted, and the agent is told so in its launch prompt.
fn run_outside_repo(
    args: RunArgs,
    folder: &std::path::Path,
    termination: &TerminationWatch,
) -> Result<i32> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    if args.intent.is_none() {
        return Err(refuse_missing_intent(&args.command).into());
    }
    if args.for_diff || args.budget.is_some() {
        anyhow::bail!(
            "not a Git repository: --for-diff and --budget shape the context packet, and \
             outside a repository there is none"
        );
    }
    let state_root = session::outside_root().with_context(|| {
        format!(
            "not a Git repository, and no state directory for a session outside one: set {} \
             or HOME to an absolute path",
            session::STATE_DIR_ENV
        )
    })?;
    if let Some(number) = termination.requested() {
        eprintln!("toimija run: termination requested before the session opened");
        return Ok(i32::try_from(128 + number).unwrap_or(1));
    }
    let mut session = session::Session::open_outside(
        &state_root,
        folder,
        &args.command,
        args.intent.clone(),
        args.scope.clone(),
    )
    .context("failed to open the toimija session")?;
    eprintln!(
        "toimija run: {} is not inside a Git repository — session {} has no context packet; \
         it is recorded under {} and listed by `toimija sessions --outside-repos`",
        folder.display(),
        session.id,
        state_root.display()
    );
    let runner = ProcessCommandRunner;
    opportunistic_sweep(&runner, &state_root);

    // No watcher means no refresh-driven heartbeat: stamp `last_seen_at` on a
    // timer instead, so a long-working agent is not reported stale.
    let stop = Arc::new(AtomicBool::new(false));
    let heartbeat = {
        let stop = Arc::clone(&stop);
        let root = state_root.clone();
        let id = session.id.clone();
        std::thread::spawn(move || {
            let mut ticks = 0u32;
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_secs(1));
                ticks += 1;
                if ticks.is_multiple_of(60)
                    && let Err(error) = session::touch(&root, &id)
                {
                    warn!(event = "toimija.session.heartbeat_failed", error = %format!("{error:#}"));
                }
            }
        })
    };
    let stop_heartbeat = || {
        stop.store(true, Ordering::Relaxed);
        let _ = heartbeat.join();
    };

    if let Some(number) = termination.requested() {
        stop_heartbeat();
        close_session_after_startup_failure(&state_root, &mut session);
        eprintln!("toimija run: termination requested before the agent launched — session closed");
        return Ok(i32::try_from(128 + number).unwrap_or(1));
    }

    let (program, rest) = args
        .command
        .split_first()
        .expect("clap `required = true` guarantees at least one argument");
    // No repo, so no repo.yaml `agents:` table: only the built-in agents.
    let agents = std::collections::BTreeMap::new();
    let mut composed = crate::adapters::agents::compose_outside_prompt(
        program,
        folder,
        args.task.as_deref(),
        &agents,
    );
    finish_prompt(&mut composed, &args);
    match &composed {
        Some(_)
            if crate::adapters::agents::idle_codex(program, rest, args.task.is_some(), &agents) =>
        {
            eprintln!(
                "toimija run: delivery: Codex session instructions; waiting for your first task"
            );
        }
        Some(_) => {
            eprintln!("toimija run: delivery: launch prompt (no packet) for `{program}`");
            say_own_codex_instructions_kept(program, rest, &args, &agents);
        }
        None => {
            eprintln!("toimija run: delivery: env-var only — not guaranteed for `{program}`");
            // No packet outside a repository, so only what the caller passed
            // can be lost here.
            let items = undelivered(false, &args);
            warn_undelivered(
                program,
                "is not a known agent",
                &items,
                "or run a known agent",
            );
            record_undelivered(&mut session, items);
        }
    }
    let handshake = session.env_outside();
    let cleared: Vec<&str> = session::RETIRED_ENV
        .iter()
        .chain(session::REPO_ONLY_ENV)
        .copied()
        .collect();
    let child_args = match crate::adapters::agents::shell_env_args(program, &handshake, &cleared)
        .and_then(|mut child_args| {
            child_args.extend_from_slice(rest);
            if let Some(prompt) = composed {
                child_args.extend(crate::adapters::agents::launch_args(
                    program,
                    rest,
                    prompt,
                    args.task.is_some(),
                    &agents,
                )?);
            }
            Ok(child_args)
        }) {
        Ok(args) => args,
        Err(error) => {
            stop_heartbeat();
            close_session_after_startup_failure(&state_root, &mut session);
            return Err(error);
        }
    };
    eprintln!(
        "toimija run: launching `{program}` (session {}, outside any repository)",
        session.id
    );
    let mut child = std::process::Command::new(program);
    child.args(&child_args).current_dir(folder).envs(handshake);
    for key in &cleared {
        child.env_remove(key);
    }
    let diagnostics_path = session.dir.join("diagnostics.log");
    let capture = match crate::diagnostics::Capture::borrow(&diagnostics_path) {
        Ok(capture) => Some(capture),
        Err(error) => {
            eprintln!(
                "toimija run: could not open the diagnostics log ({}); background output may interleave with the command: {error:#}",
                diagnostics_path.display()
            );
            None
        }
    };
    if let Some(capture) = &capture {
        child.stdout(
            capture
                .child_stdout()
                .context("failed to hand the wrapped command our stdout")?,
        );
        child.stderr(
            capture
                .child_stderr()
                .context("failed to hand the wrapped command our stderr")?,
        );
    }
    let launched = supervise_agent(&mut child, termination);
    stop_heartbeat();
    let captured = capture.map_or(0, crate::diagnostics::Capture::finish);
    if captured > 0 {
        eprintln!(
            "toimija run: captured {captured} diagnostic line(s) while the wrapped command ran — {}",
            diagnostics_path.display()
        );
    }
    let exit_status = launched
        .as_ref()
        .ok()
        .map(|status| wrapped_exit_code(*status));
    match session.close(exit_status) {
        Ok(()) => journal_closed_session(&state_root, &session),
        Err(error) => eprintln!(
            "toimija run: could not close the session marker; it stays 'active' on disk \
             and may show as a live session. Remove {}: {error:#}",
            session.dir.join("session.json").display(),
        ),
    }
    let status = launched.with_context(|| format!("failed to launch `{program}`"))?;
    Ok(wrapped_exit_code(status))
}

/// Say why a Codex launch with no task starts a turn: the user's own
/// `developer_instructions` holds the slot an idle launch would fill.
fn say_own_codex_instructions_kept(
    program: &str,
    rest: &[String],
    args: &RunArgs,
    agents: &std::collections::BTreeMap<String, crate::config::AgentPromptConfig>,
) {
    use crate::adapters::agents::{CodexStart, codex_start};
    if codex_start(program, rest, args.task.is_some(), agents) == Some(CodexStart::OwnInstructions)
    {
        eprintln!(
            "toimija run: your own `developer_instructions` is kept; the packet goes in the \
             first message instead, so Codex starts a turn"
        );
    }
}

/// Finish a composed launch prompt: the briefing at the end, after the packet
/// and the task; the alerts in front of everything, capped, saying so on
/// stderr when they were cut.
fn finish_prompt(composed: &mut Option<String>, args: &RunArgs) {
    let Some(prompt) = composed else {
        return;
    };
    if let Some(briefing) = &args.briefing {
        prompt.push_str(&format!("\n{briefing}\n"));
    }
    let (alerts, cut) = crate::adapters::agents::cap_alerts(&args.alert);
    if cut > 0 {
        eprintln!(
            "toimija run: warning: alerts exceed {} bytes; {cut} bytes were cut",
            crate::adapters::agents::ALERT_CAP_BYTES
        );
    }
    crate::adapters::agents::prepend_alerts(prompt, &alerts, cut);
}

/// What `run` had for an agent it cannot deliver to, in the order a launch
/// prompt carries it: the alerts, the packet (inside a repository), the
/// task, the caller's briefing.
fn undelivered(packet: bool, args: &RunArgs) -> Vec<String> {
    [
        (!args.alert.is_empty(), "alert"),
        (packet, "packet"),
        (args.task.is_some(), "task"),
        (args.briefing.is_some(), "briefing"),
    ]
    .into_iter()
    .filter(|(lost, _)| *lost)
    .map(|(_, item)| item.to_string())
    .collect()
}

/// Say, before the launch and while the terminal is still ours, what the
/// agent will not have in its context. Nothing lost, nothing said.
fn warn_undelivered(program: &str, why: &str, items: &[String], remedy: &str) {
    if items.is_empty() {
        return;
    }
    eprintln!(
        "toimija run: warning: `{program}` {why}; not delivered: {}",
        items.join(", ")
    );
    let handed: Vec<&str> = items
        .iter()
        .map(String::as_str)
        .filter(|item| *item != "packet")
        .collect();
    let packet = items.iter().any(|item| item == "packet");
    let mut steps = Vec::new();
    if packet {
        steps.push("point it at its packet ($TOIMIJA_LIVE)".to_string());
    }
    if !handed.is_empty() {
        steps.push(format!("give it its {} yourself", handed.join(" and ")));
    }
    eprintln!("toimija run: {}, {remedy}", steps.join(" and "));
}

/// Keep the loss on the session marker, where `sessions --json` shows it.
/// Best-effort: a marker write failing here is reported but does not stop the
/// launch.
fn record_undelivered(session: &mut session::Session, items: Vec<String>) {
    if items.is_empty() {
        return;
    }
    if let Err(error) = session.record_undelivered(items) {
        eprintln!(
            "toimija run: could not record the undelivered briefing on the session: {error:#}"
        );
    }
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
    remove_session_point(repo_root, session);
}

/// A closed session no longer stands at a repository point: remove the file
/// `FALSE_SYSTEMS_REPOSITORY_POINT_PATH` named, so nothing reads it as current.
fn remove_session_point(repo_root: &std::path::Path, session: &session::Session) {
    let _ = crate::owned::remove_file(repo_root, &session.dir.join(crate::semantic::POINT_FILE));
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
/// built only from toimija-owned state (the session marker); nothing observed
/// about the agent's behavior or file access.
struct Consumption {
    intent: String,
    intent_source: &'static str,
    expansions: usize,
}

fn collect_consumption(session: &session::Session, args: &RunArgs) -> Consumption {
    let marker = session::load_marker(&session.dir).ok();
    let launch_intent = args.intent.clone().unwrap_or_default();
    let (intent, intent_source) = match marker.as_ref().and_then(|m| m.intent.clone()) {
        Some(text) if text == launch_intent => (text, "declared at launch"),
        Some(text) => (text, "updated mid-session"),
        None => (launch_intent, "declared at launch"),
    };
    let expansions = marker
        .as_ref()
        .map(|m| m.scope.len().saturating_sub(args.scope.len()))
        .unwrap_or(0);
    Consumption {
        intent,
        intent_source,
        expansions,
    }
}

/// Bookend a `run` session: a compact, stderr-bound summary of what the wrapped
/// command did to the repo, by diffing the Git state captured on entry against
/// the state at exit. Only the deltas worth a glance are shown — a switched
/// branch, a moved HEAD (commits made), the working tree at exit, branch scope
/// since the merge-base, how many times the live packet refreshed, and what
/// the session consumed of toimija (intent, declared expansions).
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

    // Working tree as the session leaves it: the repository's work, never
    // Toimija's own files, which Git lists where `init` has not run.
    let conflicted = git::work_paths(&exit.conflicted_files).count();
    let conflicts = if conflicted == 0 {
        String::new()
    } else {
        format!(", {conflicted} conflicted")
    };
    let _ = writeln!(
        out,
        "  worktree:  {} staged, {} unstaged, {} untracked{conflicts}",
        git::work_files(&exit.staged_files).count(),
        git::work_files(&exit.unstaged_files).count(),
        git::work_paths(&exit.untracked_files).count(),
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
    let worktree = git::discover(&runner, &cwd).context("not a Git repository")?;
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
    watch::watch(&runner, &worktree, &plan, &stop, || {
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
    let (repo_root, head) = landed::locate(&runner, &cwd).context("not a Git repository")?;
    let report = landed::assess_landing(&runner, &repo_root, head, &args.base);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", render::render_landed_report(&report));
    }
    Ok(())
}

/// Both halves of a snapshot — the tree, and the token read beside it — read
/// from one worktree, behind the gates that let a tree represent the state at
/// all.
///
/// The token is fingerprinted before `materialize` and checked again after.
/// The two halves are necessarily read at different instants: `materialize`
/// runs `read-tree`, `add -A`, `rm --cached` and `write-tree` as separate
/// subprocesses, and a worktree that moves across that window would publish a
/// token read in state A beside a tree holding state B.
///
/// A move the token observes is refused here. That is the most the token can
/// give: it is a freshness check over what `git status` lists, and some changes
/// to the tree never reach that list (README, "Repository points", "Known
/// gaps"). So equal tokens never prove equal trees; a consumer that needs one
/// exact State compares the tree oid (or a repository point's id, which is
/// derived from the tree), and reads content from that immutable tree.
///
/// Both reads are of the token's inputs only: one `status` (`git::token_state`)
/// before the write, one (`git::token_state_at`) after it. A snapshot needs
/// nothing else from the repository than what its discovery found.
fn snapshot_tree_and_token(
    runner: &impl CommandRunner,
    worktree: &git::Worktree,
) -> Result<(String, Consistency)> {
    // Materialize only a *safe* state: `write-tree` should never freeze a
    // half-finished merge/rebase or a conflicted tree. Same gate as verify.
    let mut live = git::token_state(runner, worktree)?;
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
    // `tree_at_token` re-reads the token once the tree is written and requires
    // it to be what it was. A caller that wants resilience retries; a caller
    // that wants truth gets it.
    let tree =
        snapshot::tree_at_token(runner, worktree, &live, &consistency.token).map_err(|error| {
            if snapshot::is_worktree_moved(&error) {
                anyhow::anyhow!("refusing to snapshot: {error}; retry")
            } else {
                error
            }
        })?;

    Ok((tree, consistency))
}

fn run_snapshot(args: SnapshotArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    let worktree = git::discover(&runner, &cwd).context("not a Git repository")?;
    let repo_root = &worktree.root;

    // List held leases.
    if args.list {
        let leases = snapshot::list_leases(repo_root)?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&leases)?);
        } else if leases.is_empty() {
            println!("no snapshot leases");
        } else {
            for lease in &leases {
                println!(
                    "{}  tree {} ({})  {} ({})  pid {}",
                    lease.id,
                    short_oid(&lease.tree),
                    lease.tree_algorithm,
                    lease.token,
                    lease
                        .token_algorithm
                        .as_deref()
                        .unwrap_or("token version unrecorded"),
                    lease.pid
                );
            }
        }
        return Ok(());
    }

    // Release a held lease.
    if let Some(id) = &args.release {
        snapshot::release_lease(&runner, repo_root, id)?;
        println!("released snapshot lease {id}");
        return Ok(());
    }

    let (tree, consistency) = snapshot_tree_and_token(&runner, &worktree)?;
    let tree_algorithm = snapshot::TREE_ALGORITHM.id();

    if args.lease {
        // Tie the lease to the session when wrapped by `toimija run`; otherwise
        // mint a standalone id.
        let id = std::env::var("TOIMIJA_SESSION")
            .ok()
            .filter(|value| snapshot::is_valid_lease_id(value))
            .unwrap_or_else(|| {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_secs())
                    .unwrap_or(0);
                format!("{secs}-{}", std::process::id())
            });
        let lease = snapshot::acquire_lease(&runner, repo_root, &id, &tree, &consistency.token)?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&lease)?);
        } else {
            println!("leased snapshot {}", lease.id);
            println!("  tree:  {} ({})", lease.tree, lease.tree_algorithm);
            println!("  token: {} ({})", lease.token, consistency.algorithm);
            println!("  ref:   {}", snapshot::lease_ref(&lease.id));
        }
    } else if args.json {
        // `algorithm` is the token's (kept as it was); `tree_algorithm` is
        // the tree's.
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "tree": tree,
                "tree_algorithm": tree_algorithm,
                "token": consistency.token,
                "algorithm": consistency.algorithm,
                "head": consistency.head,
            }))?
        );
    } else {
        println!("snapshot tree: {tree} ({tree_algorithm})");
        println!("token: {} ({})", consistency.token, consistency.algorithm);
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
    let worktree = match git::discover(runner, cwd) {
        Ok(worktree) => worktree,
        Err(error) => {
            return VerifyReport {
                packet_path: None,
                status: VerifyStatus::UnverifiableRepository {
                    message: format!("not a Git repository: {error}"),
                },
            };
        }
    };
    let packet_path = match select_verify_packet(&worktree.root, cwd, explicit_packet) {
        Ok(path) => path,
        Err(message) => {
            return VerifyReport {
                packet_path: explicit_packet.map(std::path::Path::to_path_buf),
                status: VerifyStatus::InvalidPacket { message },
            };
        }
    };
    // Recompute the live fingerprint through the same path the packet build uses
    // (collect -> normalize), so an unchanged repo verifies as consistent. The
    // token needs one `status`; the rest of the state is read from it only when
    // the token holds and a refresh token is to be compared.
    let status = match git::read_status_of(runner, &worktree) {
        Ok(status) => status,
        Err(error) => {
            return VerifyReport {
                packet_path: Some(packet_path),
                status: VerifyStatus::UnverifiableRepository {
                    message: format!("could not read Git state: {error}"),
                },
            };
        }
    };
    let mut live = status.token_state();
    context::normalize_git_state(&mut live);
    let complete = || {
        let mut full = git::complete(runner, &worktree, status).state;
        context::normalize_git_state(&mut full);
        full
    };
    let status = verify_status(runner, &packet_path, &live, complete);
    VerifyReport {
        packet_path: Some(packet_path),
        status,
    }
}

/// The verdict on the packet at `packet_path` against `live`, the token's
/// state; `complete` reads the rest of the live state, for the refresh token.
fn verify_status(
    runner: &ProcessCommandRunner,
    packet_path: &std::path::Path,
    live: &git::GitState,
    complete: impl FnOnce() -> git::GitState,
) -> VerifyStatus {
    match load_stored_packet(packet_path) {
        StoredPacket::Ready { packet, raw } => {
            if packet.schema != context::CONTEXT_SCHEMA {
                // Asked only of a packet that is refused anyway: whether the
                // session it briefs still runs, under an older toimija.
                let live_session = packet.session.as_ref().map(|session| session.id.as_str());
                let live_session = live_session.filter(|id| {
                    sessions::named_session(runner, &live.repo_root, id)
                        == sessions::NamedSession::Live
                });
                if let Some(message) = schema_refusal(&packet.schema, live_session) {
                    return VerifyStatus::InvalidPacket { message };
                }
            }
            match crate::consistency::comparability(&packet.consistency.algorithm) {
                None => {}
                Some(Verdict::NotComparable(algorithm)) => {
                    return VerifyStatus::NotComparable {
                        packet_algorithm: algorithm,
                        algorithm: CONSISTENCY_ALGORITHM.to_string(),
                    };
                }
                Some(_) => {
                    return VerifyStatus::UnsupportedConsistencyAlgorithm {
                        algorithm: packet.consistency.algorithm,
                    };
                }
            }
            if packet.seal.is_none() {
                return VerifyStatus::InvalidPacket {
                    message: "packet has no seal".to_string(),
                };
            }
            match crate::seal::verify_value(&raw) {
                Ok(Some((expected, actual))) => {
                    return VerifyStatus::SealDigestMismatch { expected, actual };
                }
                Ok(None) => {}
                Err(error) => {
                    return VerifyStatus::InvalidPacket {
                        message: error.to_string(),
                    };
                }
            }
            let seal = packet.seal.as_ref().expect("checked above");
            if seal.schema != packet.schema {
                return VerifyStatus::InvalidPacket {
                    message: "seal schema differs from packet schema".to_string(),
                };
            }
            if seal.anchored_at != packet.consistency.token {
                return VerifyStatus::InvalidPacket {
                    message: "seal anchor differs from packet consistency token".to_string(),
                };
            }
            let artifact_digest = Some(seal.digest.clone());
            let packet_refresh_token = packet.refresh_token.clone();
            let named_session = packet.session.as_ref().map(|session| session.id.clone());
            let stored = packet.consistency;
            match stored.verify(live) {
                Verdict::Consistent => {
                    if let Some(packet_token) = packet_refresh_token {
                        let current_token = context::refresh_token(&complete());
                        if current_token != packet_token {
                            return VerifyStatus::StalePacket {
                                packet_token,
                                current_token,
                            };
                        }
                    }
                    // A packet that briefs a session is only current while
                    // that session is: a closed or killed session's id, intent
                    // and scope would otherwise read as the next agent's own.
                    if let Some(session) = named_session {
                        let state = sessions::named_session(runner, &live.repo_root, &session);
                        if state != sessions::NamedSession::Live {
                            return VerifyStatus::SessionNotLive {
                                session,
                                state: state.as_str(),
                            };
                        }
                    }
                    VerifyStatus::Verified {
                        algorithm: stored.algorithm,
                        token: stored.token,
                        head: stored.head,
                        artifact_digest,
                    }
                }
                Verdict::Stale {
                    packet_token,
                    current_token,
                } => VerifyStatus::StalePacket {
                    packet_token,
                    current_token,
                },
                Verdict::NotComparable(algorithm) => VerifyStatus::NotComparable {
                    packet_algorithm: algorithm,
                    algorithm: CONSISTENCY_ALGORITHM.to_string(),
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
        // Empty reads as unset: `run` clears it that way for Codex tools.
        if let Some(path) = std::env::var_os("TOIMIJA_LIVE_JSON").filter(|path| !path.is_empty()) {
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

/// Why a packet of `schema` is not read here, if it is not. A packet is read
/// under its own schema's rules: one an earlier toimija wrote (the
/// `toimija-context.v2` seal sat under `sauma`, say) is refused with the way
/// out; one this toimija does not know at all is refused as such.
///
/// The way out depends on who writes the packet. An older `toimija run` keeps
/// its own code after a newer toimija is installed, and its watcher rewrites
/// its session's packets in the old schema a moment after every change. So
/// for a packet that briefs a live session (`live_session`), a refresh holds
/// only until then, and the way out is to restart the session; otherwise a
/// refresh replaces the packet for good.
fn schema_refusal(schema: &str, live_session: Option<&str>) -> Option<String> {
    let current = context::CONTEXT_SCHEMA;
    if schema == current {
        None
    } else if context::EARLIER_CONTEXT_SCHEMAS.contains(&schema) {
        Some(match live_session {
            Some(session) => format!(
                "the packet is `{schema}`: an older toimija is still watching session \
                 {session} and writes its packets in that schema; this toimija reads \
                 `{current}`. `toimija refresh` replaces the packet only until its next \
                 change, so restart the session under the installed toimija (end it, and \
                 launch it again with `toimija run`)"
            ),
            None => format!(
                "the packet is `{schema}`, which an earlier toimija wrote; this toimija reads \
                 `{current}`. Run `toimija refresh` to replace it"
            ),
        })
    } else {
        Some(format!(
            "the packet is `{schema}`, which this toimija does not read; it reads `{current}`"
        ))
    }
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
    /// The packet's token is of an earlier version (`packet_algorithm`) than
    /// the one computed now (`algorithm`): neither fresh nor stale.
    NotComparable {
        packet_algorithm: String,
        algorithm: String,
    },
    UnsupportedConsistencyAlgorithm {
        algorithm: String,
    },
    StalePacket {
        packet_token: String,
        current_token: String,
    },
    SealDigestMismatch {
        expected: String,
        actual: String,
    },
    SessionNotLive {
        session: String,
        state: &'static str,
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
                            "The repository state changed after the selected packet was generated."
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
                VerifyStatus::NotComparable { .. } => {
                    map.insert(
                        "message".to_string(),
                        serde_json::Value::String(
                            "The selected packet's token is of an earlier version than the live \
                             token; tokens of different versions are not comparable, so the \
                             packet is neither fresh nor stale. Refresh it."
                                .to_string(),
                        ),
                    );
                }
                VerifyStatus::SessionNotLive { .. } => {
                    map.insert(
                        "message".to_string(),
                        serde_json::Value::String(
                            "The selected packet briefs a session that is no longer running."
                                .to_string(),
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
                format!("missing_packet: no verifiable packet at {path}; run `toimija refresh`")
            }
            VerifyStatus::InvalidPacket { message } => format!("invalid_packet: {message}"),
            VerifyStatus::MissingConsistency => {
                "missing_consistency: run `toimija refresh`".to_string()
            }
            VerifyStatus::NotComparable {
                packet_algorithm,
                algorithm,
            } => format!(
                "not_comparable: the packet's token is {packet_algorithm} and the live one \
                 {algorithm}; tokens of different versions are not comparable, so the packet is \
                 neither fresh nor stale; run `toimija refresh`"
            ),
            VerifyStatus::UnsupportedConsistencyAlgorithm { algorithm } => {
                format!("unsupported_consistency_algorithm: {algorithm}")
            }
            VerifyStatus::StalePacket { .. } => {
                "stale_packet: repository state changed after the selected packet was generated"
                    .to_string()
            }
            VerifyStatus::SealDigestMismatch { .. } => {
                "seal_digest_mismatch: selected packet was tampered with".to_string()
            }
            VerifyStatus::SessionNotLive { session, state } => format!(
                "session_not_live: the packet briefs session {session}, which is {state}; \
                 run `toimija refresh` for a packet without it"
            ),
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
    let discovered = (!args.outside_repos).then(|| git::discover_repo_root(&runner, &cwd));
    let outside = match discovered {
        None | Some(Err(crate::error::ToimijaError::NotGitRepo)) => true,
        Some(_) => false,
    };
    let repo_root = if outside {
        // Sessions `run` opened outside any repository (issue #225). Reading
        // them never creates the state directory; an absent one holds none.
        session::outside_root().with_context(|| {
            format!(
                "no state directory for sessions outside repositories: set {} or HOME to an \
                 absolute path",
                session::STATE_DIR_ENV
            )
        })?
    } else {
        discovered
            .expect("a repository was looked for")
            .context("not a Git repository")?
    };
    if outside && !args.json {
        println!(
            "Sessions outside any Git repository ({}):",
            repo_root.display()
        );
    }
    if args.prune && outside && !repo_root.is_dir() {
        println!("Nothing to prune: no session has been recorded outside a repository.");
        return Ok(());
    }
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

fn run_intent(args: IntentArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let runner = ProcessCommandRunner;
    // The wrapped agent's own session id, injected by `run`'s env handshake.
    let id = std::env::var("TOIMIJA_SESSION").map_err(|_| {
        anyhow::anyhow!(
            "TOIMIJA_SESSION is not set — `toimija intent` speaks for a wrapped session, \
             so run it from inside `toimija run -- <agent>` (the handshake provides the id)"
        )
    })?;
    let repo_root = session_root(&runner, &cwd, &id)?;
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

/// Where session `id` lives: the repository the command runs in when its
/// marker is there, else the state root when `run` opened it outside any
/// repository (issue #225) — so an agent from such a session can still speak
/// after changing into a repo. The root is found by looking, never taken from
/// the environment, so a handshake cannot steer writes elsewhere.
fn session_root(
    runner: &ProcessCommandRunner,
    cwd: &std::path::Path,
    id: &str,
) -> Result<std::path::PathBuf> {
    let repo = git::discover_repo_root(runner, cwd);
    if let Ok(repo_root) = &repo
        && session::marker_exists(repo_root, id)
    {
        return Ok(repo_root.clone());
    }
    if let Some(outside) = session::outside_root().filter(|root| session::marker_exists(root, id)) {
        return Ok(outside);
    }
    // Neither holds it: let the repository path report the missing marker.
    repo.context("not a Git repository")
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

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
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
    use std::path::{Path, PathBuf};

    /// Answers the reads `snapshot_tree_and_token` makes, and lets the worktree
    /// move exactly once — when `write-tree` runs, which is the middle of
    /// `materialize` and the widest part of the window.
    struct MovingRunner {
        moved: std::cell::Cell<bool>,
        move_on_write_tree: bool,
    }

    impl MovingRunner {
        fn new(move_on_write_tree: bool) -> Self {
            Self {
                moved: std::cell::Cell::new(false),
                move_on_write_tree,
            }
        }

        fn reply(&self, args: &[&str]) -> crate::command::CommandOutput {
            let ok = |stdout: &str| crate::command::CommandOutput {
                status: 0,
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            };
            match args {
                ["rev-parse", "--show-toplevel"] => ok("/repo\n"),
                ["rev-parse", "--git-dir"] => ok("/repo/.git\n"),
                ["rev-parse", "HEAD"] => ok("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"),
                ["symbolic-ref", ..] => ok("main\n"),
                ["write-tree"] => {
                    if self.move_on_write_tree {
                        self.moved.set(true);
                    }
                    ok("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n")
                }
                // A file appears in the worktree partway through materializing.
                args if args.first() == Some(&"--no-optional-locks") => {
                    if self.moved.get() {
                        ok(
                            "# branch.oid aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0# branch.head main\0? appeared.txt\0",
                        )
                    } else {
                        ok(
                            "# branch.oid aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0# branch.head main\0",
                        )
                    }
                }
                // Everything else (read-tree, add, rm, merge-base, base
                // discovery) either succeeds emptily or is allowed to fail.
                _ => ok(""),
            }
        }
    }

    impl CommandRunner for MovingRunner {
        fn run(
            &self,
            _cwd: &Path,
            program: &str,
            args: &[&str],
        ) -> std::io::Result<crate::command::CommandOutput> {
            assert_eq!(program, "git");
            Ok(self.reply(args))
        }

        fn run_with_env(
            &self,
            _cwd: &Path,
            program: &str,
            args: &[&str],
            _env: &[(&str, &std::ffi::OsStr)],
        ) -> std::io::Result<crate::command::CommandOutput> {
            assert_eq!(program, "git");
            Ok(self.reply(args))
        }
    }

    #[test]
    fn a_worktree_that_moves_while_materializing_is_refused() {
        let repo = PathBuf::from("/repo");

        // Quiet repo: the tree and the token describe the same state, so the
        // pair is publishable.
        let still = MovingRunner::new(false);
        let (tree, consistency) =
            snapshot_tree_and_token(&still, &git::Worktree::at_root(repo.clone()))
                .expect("a quiet repository still leases");
        assert_eq!(tree, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert!(!consistency.token.is_empty());

        // The same run, with a file appearing while the tree is written. The
        // token would attest the state before it, so the pair is refused
        // rather than published.
        let moving = MovingRunner::new(true);
        let error = snapshot_tree_and_token(&moving, &git::Worktree::at_root(repo.clone()))
            .expect_err("a worktree that moved mid-materialize must not publish a pair");
        let message = error.to_string();
        assert!(
            message.contains("changed while the tree was being written"),
            "expected the pairing refusal, got {message:?}"
        );
    }

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
            ahead: None,
            behind: None,
            ahead_behind_ref: None,
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

        let busy = Consumption {
            intent: "widened".to_string(),
            intent_source: "updated mid-session",
            expansions: 2,
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
