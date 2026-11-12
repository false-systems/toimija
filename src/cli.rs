use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use tracing::{info, info_span, warn};

use crate::command::{CommandRunner, ProcessCommandRunner};
use crate::config::{self, RepoConfig};
use crate::context;
use crate::error::ToimijaError;
use crate::git;
use crate::logging::LogFormat;
use crate::render;
use crate::session;
use crate::sessions;
use crate::workspace;

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
    about = "Tells a coding agent where the work stands",
    long_about = "Tells a coding agent where the work stands.\n\n\
        Toimija reads Git's state — branch, HEAD, merge-base, staged, unstaged and \
        untracked files — and works out a short packet an agent reads before it \
        touches the repository: what changed, what to read first, who else is \
        working here and why, and the last handover.",
    after_help = "Typical agent workflow:\n  \
        toimija run claude                         # start an agent with the packet in its prompt\n  \
        toimija enter --for-diff --budget 4000     # an agent started another way: the packet\n  \
        toimija sessions                           # who else is working here, and why\n  \
        toimija intent \"<why>\" --scope <path>      # a session says why it is here\n  \
        toimija handover --next \"<note>\"           # a note for the next agent\n\n\
        Boundaries: Toimija reports facts and refuses to decide. It is local-only: \
        it writes only under .toimija/ (session markers and the handover) and, for \
        sessions outside any repository, its state directory; it runs no checks, \
        starts no daemons, and makes no network calls of its own."
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
    /// Start an agent as a session, with the packet in its launch prompt, and
    /// exit with its status.
    ///
    /// Use this to start any agent command: `toimija run claude`. It works out
    /// the packet, writes a session marker under `.toimija/sessions/<id>/` so
    /// other sessions see this one, starts the agent, and exits with the
    /// agent's status (128+signal preserved). While the agent runs, toimija
    /// starts no git process: it stamps the marker once a minute, and removes
    /// it when the agent exits. Known agents (claude, codex, gemini, pi,
    /// opencode) and those configured under `agents:` in `.toimija/repo.yaml`
    /// get the packet in their launch prompt — pass the task via `--task`;
    /// other commands get only the TOIMIJA_* environment, and `run` says so.
    /// `--intent` says why the session exists; without it other sessions see
    /// `unknown` until `toimija intent` says. `--scope` tells other sessions
    /// what you expect to touch — advisory, never an assignment.
    ///
    /// Outside any Git repository it opens a session with no packet instead
    /// (in a workspace, with the workspace packet): the marker lives in the
    /// machine-local state directory (`$TOIMIJA_STATE_DIR`, else
    /// `$XDG_STATE_HOME/toimija`, else `~/.local/state/toimija`), never in the
    /// folder, and `toimija sessions --outside-repos` lists it.
    #[command(
        after_help = "Examples:\n  toimija run claude   # a session; say why later with `toimija intent`\n  \
        toimija run --intent \"fix the redirect\" --task \"fix the login redirect\" -- claude   # packet + task in the launch prompt\n  \
        toimija run --intent \"fix the redirect\" --scope src/cli.rs -- codex   # declare intent/scope for other sessions\n  \
        toimija run --intent unknown --for-diff --budget 4000 -- sh -c '…'   # tune the packet the agent sees\n  \
        cd ~/projects && toimija run --intent \"cross-repo sweep\" --scope ~/projects/a -- claude   # outside a repo: a session, no packet"
    )]
    Run(RunArgs),
    /// Work out the packet and print it: where the work stands.
    ///
    /// Use this as an agent's first step when `toimija run` did not start it,
    /// and again whenever it needs the state as it is now: branch, HEAD,
    /// merge-base, changed files, what to read first, the validation plan and
    /// the last handover. The packet is worked out each time it is asked for
    /// and never written to disk, so it cannot go stale. Under `toimija run`
    /// it carries the session's intent and scope. In a workspace folder it
    /// prints the workspace packet. Read-only; runs no checks, no network.
    #[command(
        after_help = "Examples:\n  toimija enter --for-diff --budget 4000   # the standard agent preflight\n  \
        toimija enter --json                     # the whole packet as JSON\n  \
        toimija enter --identity --json          # only what the repository says it is"
    )]
    Enter(EnterArgs),
    /// List the sessions working here: who, on which branch and worktree, with
    /// what intent and scope, and where they overlap.
    ///
    /// Use this before starting work in a shared repository: it lists the live
    /// sessions, and stale ones (open, but not seen for 90 minutes), with
    /// descriptive overlap warnings. A session whose process is gone is over
    /// and is not listed. Read-only: assigns nothing, signals nothing, no
    /// network. Outside a repository, or with `--outside-repos`, it lists the
    /// sessions `run` opened outside any repository instead.
    #[command(
        after_help = "Examples:\n  toimija sessions                   # LIVE / STALE listing + warnings\n  \
        toimija sessions --json            # machine-readable report\n  \
        toimija sessions --outside-repos   # sessions opened outside any repository"
    )]
    Sessions(SessionsArgs),
    /// Declare or update this session's intent — tell other sessions why you are here.
    ///
    /// Use this from inside a `toimija run` session (the agent itself,
    /// mid-work): the session id comes from the TOIMIJA_SESSION handshake.
    /// Sets the marker's intent and appends any `--scope` paths, so `toimija
    /// sessions` shows the others why this session exists. Advisory — never an
    /// assignment, never a lock. Writes one marker file; no network.
    #[command(
        after_help = "Examples:\n  toimija intent \"migrating the auth module\"\n  \
        toimija intent \"fix the redirect\" --scope src/cli.rs --scope tests/cli_run.rs"
    )]
    Intent(IntentArgs),
    /// Leave a handover for the next agent in this working copy.
    ///
    /// Use this before you finish: what is unfinished, what the next agent
    /// should know, what surprised you. It replaces the handover before it,
    /// and every packet worked out here shows it under "Last handover", so the
    /// next agent reads it on entering (`toimija enter`, or the launch prompt
    /// of `toimija run`). Works with or without a session. Writes one file,
    /// `.toimija/handover.json`; no network.
    #[command(
        after_help = "Examples:\n  toimija handover --unfinished \"the parser's error tests\" \
        --next \"the lexer is done; start at src/parse.rs\"\n  \
        toimija handover --surprised \"the build writes to a shared target folder\""
    )]
    Handover(HandoverArgs),
}

#[derive(Debug, Args)]
pub struct EnterArgs {
    /// Emit the packet as JSON instead of Markdown.
    #[arg(long)]
    pub json: bool,
    /// Add a diff-focused section: committed, staged, unstaged, untracked, conflicts.
    #[arg(long)]
    pub for_diff: bool,
    /// Approximate output cap in UTF-8 bytes; optional sections are omitted atomically.
    #[arg(long)]
    pub budget: Option<usize>,
    /// Emit only the packet's `repo` object — what `.toimija/repo.yaml` says
    /// the repository is — as `toimija-identity.v1`, read without collecting
    /// Git state or naming a session. Requires `--json`.
    #[arg(long, requires = "json", conflicts_with_all = ["for_diff", "budget"])]
    pub identity: bool,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Diff-focused packet: committed, staged, unstaged, untracked, conflicts.
    #[arg(long)]
    pub for_diff: bool,
    /// Output budget for the Markdown packet.
    #[arg(long)]
    pub budget: Option<usize>,
    /// Free-text declaration of why this session is here, shown to other
    /// sessions by `toimija sessions`. Advisory only — never an assignment.
    #[arg(long)]
    pub intent: Option<String>,
    /// A path this session expects to touch (repeatable). Advisory only — never a
    /// lock. Absence renders as `unknown`.
    #[arg(long = "scope", value_name = "PATH")]
    pub scope: Vec<String>,
    /// The task for the agent, delivered inside the composed launch prompt of a
    /// known agent (`claude`, `codex`, `gemini`, `pi`, `opencode`) or one configured under
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

/// Without this check `--briefing --task "fix it"` takes `--task` as the
/// briefing and launches a program named `fix it`. When the argument after a spaced
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
    /// Free-text declaration of why this session is here, shown to other
    /// sessions by `toimija sessions`.
    pub text: String,
    /// A path this session expects to touch (repeatable, appended to the
    /// declared scope). Advisory only — never a lock.
    #[arg(long = "scope", value_name = "PATH")]
    pub scope: Vec<String>,
}

#[derive(Debug, Args)]
pub struct HandoverArgs {
    /// What is not done, and where to pick it up.
    #[arg(long, value_name = "TEXT")]
    pub unfinished: Option<String>,
    /// What the next agent should know.
    #[arg(long, value_name = "TEXT")]
    pub next: Option<String>,
    /// What caught you out.
    #[arg(long, value_name = "TEXT")]
    pub surprised: Option<String>,
}

#[derive(Debug, Args)]
pub struct SessionsArgs {
    /// Emit the report as JSON instead of the human-readable listing.
    #[arg(long)]
    pub json: bool,
    /// List the sessions `run` opened outside any Git repository (kept in the
    /// machine-local state directory) instead of this repository's. The
    /// default when not inside a repository.
    #[arg(long)]
    pub outside_repos: bool,
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

/// Dispatch the parsed command and return the process exit code. `run` picks
/// its own code (the agent's); everything else maps success to 0. Returning
/// the code — instead of calling `process::exit` inside a handler — keeps the
/// `toimija.command.finish` bookend and the span guard on every exit path.
pub fn run(cli: Cli) -> Result<i32> {
    let started = Instant::now();
    let command_name = cli.command.name();
    let output_format = match &cli.command {
        Command::Enter(args) if args.json => "json",
        Command::Sessions(args) if args.json => "json",
        _ => "text",
    };
    let span = info_span!(
        "toimija.command",
        command = command_name,
        output_format,
        config_path = config::CONFIG_PATH
    );
    let _guard = span.enter();
    info!(event = "toimija.command.start");

    let result = match cli.command {
        Command::Run(args) => run_run(args),
        Command::Enter(args) => run_enter(args).map(|()| 0),
        Command::Sessions(args) => run_sessions(args).map(|()| 0),
        Command::Intent(args) => run_intent(args).map(|()| 0),
        Command::Handover(args) => run_handover(args).map(|()| 0),
    };

    // A failure's finish event is at info, beside the start event: `main`
    // says the error to the person once, and `-v` shows both bookends.
    match &result {
        Ok(exit_code) => info!(
            event = "toimija.command.finish",
            duration_ms = started.elapsed().as_millis() as u64,
            exit_code
        ),
        Err(error) => info!(
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
            Command::Run(_) => "run",
            Command::Enter(_) => "enter",
            Command::Sessions(_) => "sessions",
            Command::Intent(_) => "intent",
            Command::Handover(_) => "handover",
        }
    }
}

/// The folder the command runs in. A terminal can sit in a folder that was
/// deleted after it moved in, such as a removed worktree; that is said in
/// words, never as the bare "No such file or directory".
fn current_dir() -> Result<std::path::PathBuf> {
    std::env::current_dir().map_err(|error| folder_unreadable(error, std::env::var_os("PWD")))
}

fn folder_unreadable(error: std::io::Error, pwd: Option<std::ffi::OsString>) -> anyhow::Error {
    if error.kind() != std::io::ErrorKind::NotFound {
        return anyhow::Error::new(error).context("cannot read the folder you are in");
    }
    // PWD is the shell's word for where it is. It is named only when that
    // path is gone too, so a stale or unrelated PWD names nothing.
    let folder = pwd
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute() && !path.exists())
        .map(|path| format!(" ({})", path.display()))
        .unwrap_or_default();
    anyhow::anyhow!(
        "the folder you are in{folder} no longer exists, it was deleted or moved: \
         cd into a folder that exists and run it again"
    )
}

/// A session may start without saying why: said once, before the agent has the
/// terminal, with how to say it. Missing stays missing, never a made-up intent.
fn say_no_intent(args: &RunArgs) {
    if args.intent.is_none() {
        eprintln!(
            "toimija run: no intent given: other sessions see `unknown` until one is declared \
             (`toimija intent \"<why>\"`, or `--intent` at launch)"
        );
    }
}

fn run_enter(args: EnterArgs) -> Result<()> {
    let cwd = current_dir()?;
    let runner = ProcessCommandRunner;
    if args.identity {
        return print_identity(&runner, &cwd);
    }
    let worktree = match git::discover(&runner, &cwd) {
        Ok(worktree) => worktree,
        // Outside any repository: the workspace's packet, from files.
        Err(ToimijaError::NotGitRepo) => return print_workspace_packet(&runner, &cwd, &args),
        Err(error) => return Err(error).context("failed to work out the packet"),
    };
    let mut packet = context::work_out(&runner, &worktree, args.for_diff)?;
    context::for_ambient_session(&mut packet);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&packet)?);
    } else {
        println!("{}", render_packet(&packet, args.budget));
    }
    Ok(())
}

/// What the repository says it is, from `.toimija/repo.yaml` alone: one git
/// process finds the root, and nothing is written or registered. A tool that
/// asks only that (ohjaa's perimeter) pays for no Git state.
fn print_identity(runner: &ProcessCommandRunner, cwd: &std::path::Path) -> Result<()> {
    let repo_root = git::discover_repo_root(runner, cwd)
        .map_err(anyhow::Error::from)
        .context("failed to build context packet")?;
    let identity = context::identity(&repo_root).context("failed to build context packet")?;
    println!("{}", serde_json::to_string_pretty(&identity)?);
    Ok(())
}

/// The packet as Markdown, capped at `budget`, else the repository's
/// configured budget.
fn render_packet(packet: &context::ContextPacket, budget: Option<usize>) -> String {
    let budget = budget
        .or(packet.config_default_budget())
        .unwrap_or(config::DEFAULT_BUDGET);
    render::render_context_markdown(packet, budget)
}

/// Outside any repository: print the packet of the workspace the current
/// directory is in. Built from files, with no git process. With no workspace
/// above, the error says how to make one.
fn print_workspace_packet(
    runner: &ProcessCommandRunner,
    cwd: &std::path::Path,
    args: &EnterArgs,
) -> Result<()> {
    let Some(root) = workspace::find(cwd) else {
        anyhow::bail!(
            "not a Git repository, and no workspace above it: move into a repository, or \
             write {} to make this folder a workspace",
            workspace::WORKSPACE_PATH
        );
    };
    if args.for_diff || args.budget.is_some() {
        anyhow::bail!(
            "{} is a workspace, not a Git repository: --for-diff and --budget shape a \
             repository's packet",
            root.display()
        );
    }
    let packet = workspace::build(runner, &root)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&packet)?);
    } else {
        print!("{}", workspace::render_markdown(&packet));
    }
    Ok(())
}

/// How often `run` stamps its session marker while the agent runs: a file
/// write, never a git process.
const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(60);

fn run_run(mut args: RunArgs) -> Result<i32> {
    args.task = args.task.filter(|task| !task.trim().is_empty());
    args.briefing = args.briefing.filter(|briefing| !briefing.trim().is_empty());
    args.alert.retain(|alert| !alert.trim().is_empty());

    // Armed before a session can exist, not after: a signal that arrives
    // while the marker is being written must not kill the wrapper and strand
    // the marker of a session that never ran.
    let termination =
        TerminationWatch::install().context("failed to register termination signal handlers")?;

    let cwd = current_dir()?;
    let runner = ProcessCommandRunner;
    let worktree = match git::discover(&runner, &cwd) {
        Ok(worktree) => worktree,
        // Git ran and said no: a session outside any repository.
        // A missing git stays an error — that is not the same as "no repo".
        Err(ToimijaError::NotGitRepo) => {
            return run_outside_repo(args, &cwd, &termination);
        }
        Err(error) => return Err(error).context("not a Git repository"),
    };
    let repo_root = worktree.root.clone();
    say_no_intent(&args);
    // The packet first: a repository whose state cannot be read starts no
    // session and no agent.
    let mut packet = context::work_out(&runner, &worktree, args.for_diff)?;
    let repo_config = RepoConfig::load_optional(&repo_root)?
        .unwrap_or_else(|| RepoConfig::default_for(&repo_root));
    // Asked to stop before any marker exists: leave without creating one.
    if let Some(number) = termination.requested() {
        eprintln!("toimija run: termination requested before the session opened");
        return Ok(i32::try_from(128 + number).unwrap_or(1));
    }

    let mut session = session::Session::open(
        &repo_root,
        &args.command,
        args.intent.clone(),
        args.scope.clone(),
    )
    .context("failed to open the toimija session")?;
    // The branch and HEAD at entry stay on the marker, so a later packet can
    // show drift.
    if let Err(error) = session.record_git(packet.git.branch.clone(), packet.git.head_oid.clone()) {
        end_after_startup_failure(session);
        return Err(error);
    }
    packet.session = Some(context::SessionBlock::of(session.marker()));
    prune_ended_beside(&runner, &repo_root);
    let markdown = render_packet(&packet, args.budget);
    println!("{markdown}");

    if let Some(number) = termination.requested() {
        end_after_startup_failure(session);
        eprintln!("toimija run: termination requested before the agent launched — session ended");
        return Ok(i32::try_from(128 + number).unwrap_or(1));
    }

    let (program, rest) = args
        .command
        .split_first()
        .expect("clap `required = true` guarantees at least one argument");
    // For a known or configured agent the packet travels in argv — in context
    // by construction. Anything else gets only the environment handshake, and
    // says so instead of silently degrading. The `agents:` section of
    // repo.yaml extends the built-in table, so any harness can be taught
    // delivery without patching this crate.
    let mut composed = crate::adapters::agents::compose_launch_prompt(
        program,
        &markdown,
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
    // The prompt travels where the agent reads it (`launch_args`): last
    // positional argument, or after the flag a built-in takes or its `agents:`
    // entry names.
    let handshake = session.env();
    let child_args = match crate::adapters::agents::shell_env_args(program, &handshake, &[])
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
        Ok(child_args) => child_args,
        Err(error) => {
            end_after_startup_failure(session);
            return Err(error);
        }
    };
    eprintln!(
        "toimija run: launching `{program}` (session {})",
        session.id
    );
    let mut child = std::process::Command::new(program);
    child
        .args(&child_args)
        .current_dir(&repo_root)
        .envs(handshake);
    let mut heartbeat = Heartbeat::new(&repo_root, &session.id);
    let launched = supervise_agent(&mut child, &termination, || heartbeat.beat());
    heartbeat.report();

    // The agent is gone: so is its session. What it declared while it ran
    // comes back with the marker, for the summary.
    let ended = end_session(session);
    if launched.is_ok() {
        // One `git status`: where the agent left the work.
        match git::status_state(&runner, &worktree) {
            Ok(exit) => eprint!(
                "\n{}",
                render_run_summary(&packet.git, &exit, &consumption(ended.as_ref(), &args))
            ),
            Err(error) => eprintln!("toimija run: could not compute exit summary: {error:#}"),
        }
    }
    let status = launched.with_context(|| format!("failed to launch `{program}`"))?;
    Ok(wrapped_exit_code(status))
}

/// `run` outside any Git repository: a visible session with no packet (in a
/// workspace, the workspace's packet). The marker lives under the
/// machine-local state root, never in `folder`, and `toimija sessions
/// --outside-repos` lists it.
fn run_outside_repo(
    args: RunArgs,
    folder: &std::path::Path,
    termination: &TerminationWatch,
) -> Result<i32> {
    say_no_intent(&args);
    if args.for_diff || args.budget.is_some() {
        anyhow::bail!(
            "not a Git repository: --for-diff and --budget shape the context packet, and \
             outside a repository there is none"
        );
    }
    // In a workspace the launch prompt carries the workspace packet. It is
    // read before the session opens, so a workspace file that does not parse
    // stops the launch instead of starting an agent with no context.
    let runner = ProcessCommandRunner;
    let workspace_packet = match workspace::find(folder) {
        Some(root) => Some(
            workspace::build(&runner, &root)
                .map(|packet| workspace::render_markdown(&packet))
                .context("failed to build the workspace packet")?,
        ),
        None => None,
    };
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
    if workspace_packet.is_some() {
        eprintln!(
            "toimija run: {} is in a workspace, not a Git repository — session {} gets the \
             workspace packet; it is recorded under {} and listed by `toimija sessions \
             --outside-repos`",
            folder.display(),
            session.id,
            state_root.display()
        );
    } else {
        eprintln!(
            "toimija run: {} is not inside a Git repository — session {} has no context packet; \
             it is recorded under {} and listed by `toimija sessions --outside-repos`",
            folder.display(),
            session.id,
            state_root.display()
        );
    }
    prune_ended_beside(&runner, &state_root);

    if let Some(number) = termination.requested() {
        end_after_startup_failure(session);
        eprintln!("toimija run: termination requested before the agent launched — session ended");
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
        workspace_packet.as_deref(),
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
            if workspace_packet.is_some() {
                eprintln!(
                    "toimija run: delivery: launch prompt with the workspace packet inlined for `{program}`"
                );
            } else {
                eprintln!("toimija run: delivery: launch prompt (no packet) for `{program}`");
            }
            say_own_codex_instructions_kept(program, rest, &args, &agents);
        }
        None => {
            eprintln!("toimija run: delivery: env-var only — not guaranteed for `{program}`");
            // No packet outside a repository, so only what the caller passed
            // can be lost here.
            let items = undelivered(workspace_packet.is_some(), &args);
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
    let cleared: Vec<&str> = session::REPO_ONLY_ENV.to_vec();
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
        Ok(child_args) => child_args,
        Err(error) => {
            end_after_startup_failure(session);
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
    let mut heartbeat = Heartbeat::new(&state_root, &session.id);
    let launched = supervise_agent(&mut child, termination, || heartbeat.beat());
    heartbeat.report();
    end_session(session);
    let status = launched.with_context(|| format!("failed to launch `{program}`"))?;
    Ok(wrapped_exit_code(status))
}

/// Stamps the session marker while the agent runs. A failure is kept, not
/// printed, while the agent owns the terminal, and reported once it exits.
struct Heartbeat<'a> {
    root: &'a std::path::Path,
    id: &'a str,
    failures: usize,
    last_error: Option<String>,
}

impl<'a> Heartbeat<'a> {
    fn new(root: &'a std::path::Path, id: &'a str) -> Self {
        Self {
            root,
            id,
            failures: 0,
            last_error: None,
        }
    }

    fn beat(&mut self) {
        if let Err(error) = session::touch(self.root, self.id) {
            self.failures += 1;
            self.last_error = Some(format!("{error:#}"));
        }
    }

    fn report(&self) {
        if let Some(error) = &self.last_error {
            eprintln!(
                "toimija run: the session marker could not be stamped {} time(s), so other \
                 sessions may have seen this one as stale: {error}",
                self.failures
            );
        }
    }
}

/// Remove the markers beside this session whose process is provably gone:
/// sessions whose `run` was killed before it could remove its own. Best
/// effort: a failure is logged, and never stops the launch.
fn prune_ended_beside(runner: &ProcessCommandRunner, root: &std::path::Path) {
    if let Err(error) = sessions::prune_ended(runner, root) {
        warn!(event = "toimija.session.prune_failed", error = %format!("{error:#}"));
    }
}

/// End the session: its marker is removed, so no reader takes it for a
/// working one. Returns the marker as it stood last, or `None` when it could
/// not be removed, which is said.
fn end_session(session: session::Session) -> Option<session::SessionMarker> {
    let marker_path = session.dir.join("session.json");
    match session.end() {
        Ok(marker) => Some(marker),
        Err(error) => {
            eprintln!(
                "toimija run: could not remove the session marker; it stays on disk until its \
                 process is seen gone. Remove {}: {error:#}",
                marker_path.display()
            );
            None
        }
    }
}

fn end_after_startup_failure(session: session::Session) {
    end_session(session);
}

/// Termination signals set a flag instead of killing the wrapper, for as long
/// as this value lives.
///
/// Registration has to begin before a session marker exists on disk, not when
/// the agent launches. A signal arriving while the session opens would
/// otherwise kill toimija outright and leave the marker of a session that never
/// ran, until a later `run` beside it sees its process gone.
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
/// this PID specifically. Registering SIGINT also means an interrupt does not
/// kill the wrapper before it can remove the session marker and print the exit
/// summary.
///
/// While the agent runs, `heartbeat` is called once every [`HEARTBEAT`]: the
/// only work this loop does besides waiting, and it starts no process.
#[cfg(unix)]
fn supervise_agent(
    command: &mut std::process::Command,
    termination: &TerminationWatch,
    mut heartbeat: impl FnMut(),
) -> std::io::Result<std::process::ExitStatus> {
    let mut child = command.spawn()?;
    let mut forwarded_at: Option<Instant> = None;
    let mut beat_at = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        if beat_at.elapsed() >= HEARTBEAT {
            heartbeat();
            beat_at = Instant::now();
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
    _heartbeat: impl FnMut(),
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
        steps.push("have it run `toimija enter` for its packet".to_string());
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

/// What the session consumed of toimija — and nothing else: built only from
/// toimija-owned state (the session marker); nothing observed about the
/// agent's behavior or file access.
struct Consumption {
    /// The intent and where it was said; `None` when none was ever given.
    intent: Option<(String, &'static str)>,
    expansions: usize,
}

/// `ended` is the marker as the session left it; `None` when it could not be
/// read back, and then only what was given at launch is known.
fn consumption(ended: Option<&session::SessionMarker>, args: &RunArgs) -> Consumption {
    let said = ended.and_then(|marker| marker.intent.clone());
    let intent = match (said, args.intent.clone()) {
        (Some(text), Some(launch)) if text == launch => Some((text, "declared at launch")),
        (Some(text), Some(_)) => Some((text, "updated mid-session")),
        (Some(text), None) => Some((text, "declared mid-session")),
        (None, launch) => launch.map(|text| (text, "declared at launch")),
    };
    let expansions = ended
        .map(|marker| marker.scope.len().saturating_sub(args.scope.len()))
        .unwrap_or(0);
    Consumption { intent, expansions }
}

/// Bookend a `run` session: a compact, stderr-bound summary of what the wrapped
/// command did to the repo, by comparing the Git state at entry with the one
/// `git status` read at exit. Only the deltas worth a glance are shown — a
/// switched branch, a moved HEAD (commits made), the working tree at exit,
/// and what the session consumed of toimija (intent, declared expansions).
fn render_run_summary(
    entry: &git::GitState,
    exit: &git::GitState,
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
    // Toimija's own files, which Git lists where they are not ignored.
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

    match &consumption.intent {
        Some((intent, source)) => {
            let _ = writeln!(out, "  intent:    \"{intent}\" ({source})");
        }
        None => {
            let _ = writeln!(out, "  intent:    not given");
        }
    }
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
    let end = oid.char_indices().nth(7).map_or(oid.len(), |(at, _)| at);
    &oid[..end]
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

fn run_sessions(args: SessionsArgs) -> Result<()> {
    let cwd = current_dir()?;
    let runner = ProcessCommandRunner;
    let discovered = (!args.outside_repos).then(|| git::discover_repo_root(&runner, &cwd));
    let outside = matches!(discovered, None | Some(Err(ToimijaError::NotGitRepo)));
    let root = if outside {
        // Sessions `run` opened outside any repository. Reading them never
        // creates the state directory; an absent one holds none.
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
        println!("Sessions outside any Git repository ({}):", root.display());
    }
    let report = sessions::report(&runner, &root)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", sessions::render_text(&report));
    }
    Ok(())
}

fn run_intent(args: IntentArgs) -> Result<()> {
    let cwd = current_dir()?;
    let runner = ProcessCommandRunner;
    // The wrapped agent's own session id, injected by `run`'s env handshake.
    let id = std::env::var("TOIMIJA_SESSION")
        .ok()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "TOIMIJA_SESSION is not set — `toimija intent` speaks for a wrapped session, \
                 so run it from inside `toimija run -- <agent>` (the handshake provides the id)"
            )
        })?;
    let root = session_root(&runner, &cwd, &id)?;
    let marker = session::declare_intent(&root, &id, args.text, args.scope)?;
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
/// repository — so an agent from such a session can still speak
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

fn run_handover(args: HandoverArgs) -> Result<()> {
    let given = |text: Option<String>| {
        text.map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    };
    let (unfinished, next, surprised) = (
        given(args.unfinished),
        given(args.next),
        given(args.surprised),
    );
    anyhow::ensure!(
        unfinished.is_some() || next.is_some() || surprised.is_some(),
        "say at least one thing for the next agent: --unfinished, --next or --surprised"
    );
    let cwd = current_dir()?;
    let runner = ProcessCommandRunner;
    let repo_root = git::discover_repo_root(&runner, &cwd).context("not a Git repository")?;
    // HEAD's id, then its branch: `--abbrev-ref` applies to the arguments
    // after it. An unborn HEAD answers neither; a detached one names no branch.
    let (head, branch) = match runner.run(
        &repo_root,
        "git",
        &["rev-parse", "HEAD", "--abbrev-ref", "HEAD"],
    ) {
        Ok(output) if output.status == 0 => {
            let text = output.stdout_lossy();
            let mut lines = text.lines().map(str::trim);
            let head = lines
                .next()
                .filter(|line| !line.is_empty())
                .map(str::to_owned);
            let branch = lines
                .next()
                .filter(|line| !line.is_empty() && *line != "HEAD")
                .map(str::to_owned);
            (head, branch)
        }
        _ => (None, None),
    };
    let handover = session::Handover {
        written_at: session::now_secs(),
        session: session::ambient_session(&repo_root).map(|(id, _)| id),
        branch,
        head,
        unfinished,
        next,
        surprised,
    };
    session::write_handover(&repo_root, &handover)?;
    info!(event = "toimija.handover.written", path = %repo_root.join(session::HANDOVER_FILE).display());
    println!(
        "Handover written for the next agent in {}: {}",
        repo_root.display(),
        [
            handover.unfinished.as_ref().map(|_| "unfinished"),
            handover.next.as_ref().map(|_| "for the next agent"),
            handover.surprised.as_ref().map(|_| "surprised by"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitState;
    use std::path::PathBuf;

    fn not_found() -> std::io::Error {
        std::io::Error::from(std::io::ErrorKind::NotFound)
    }

    #[test]
    fn a_deleted_folder_is_named_from_pwd_only_when_that_path_is_gone() {
        let parent = tempfile::tempdir().unwrap();
        let gone = parent.path().join("removed-worktree");
        let said = format!(
            "{:#}",
            folder_unreadable(not_found(), Some(gone.clone().into()))
        );
        assert_eq!(
            said,
            format!(
                "the folder you are in ({}) no longer exists, it was deleted or moved: \
                 cd into a folder that exists and run it again",
                gone.display()
            )
        );

        // A PWD that still exists, is relative or is missing names nothing.
        for pwd in [
            Some(parent.path().as_os_str().to_owned()),
            Some("gone".into()),
            None,
        ] {
            let said = format!("{:#}", folder_unreadable(not_found(), pwd.clone()));
            assert!(
                said.starts_with("the folder you are in no longer exists"),
                "{pwd:?}: {said}"
            );
        }
    }

    #[test]
    fn an_unreadable_folder_that_is_not_gone_keeps_the_os_reason() {
        let error = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let said = format!("{:#}", folder_unreadable(error, None));
        assert!(
            said.starts_with("cannot read the folder you are in: "),
            "{said}"
        );
        assert!(!said.contains("no longer exists"), "{said}");
    }

    /// `run --help` names the built-in agents twice, in the command's
    /// description and in `--task`'s: both lists are the table, in its
    /// order, so neither falls behind when an agent is added.
    #[test]
    fn run_help_names_every_builtin_agent() {
        use clap::CommandFactory;
        let mut run = Cli::command()
            .find_subcommand("run")
            .expect("run is a subcommand")
            .clone();
        let help = run.render_long_help().to_string();
        let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
        let listed = |opening: &str| -> Vec<String> {
            let start = help
                .find(opening)
                .unwrap_or_else(|| panic!("no {opening:?} in: {help}"))
                + opening.len();
            let end = start + help[start..].find(')').expect("a closing parenthesis");
            help[start..end]
                .split(", ")
                .map(|name| name.trim_matches('`').to_owned())
                .collect()
        };
        let builtin: Vec<String> = crate::adapters::agents::builtin_agents()
            .map(str::to_owned)
            .collect();
        assert_eq!(listed("Known agents ("), builtin);
        assert_eq!(listed("of a known agent ("), builtin);
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
            intent: Some(("test".to_string(), "declared at launch")),
            expansions: 0,
        }
    }

    #[test]
    fn summary_flags_a_moved_head_and_branch_switch() {
        let entry = state("main", "aaaaaaaaaa");
        let exit = state("feature", "bbbbbbbbbb");
        let out = render_run_summary(&entry, &exit, &quiet_consumption());
        assert!(out.contains("branch:    main → feature"), "{out}");
        assert!(
            out.contains("HEAD:      aaaaaaa → bbbbbbb (moved)"),
            "{out}"
        );
    }

    #[test]
    fn summary_omits_head_line_when_head_is_unchanged() {
        let unchanged = state("main", "abc1234");
        let out = render_run_summary(&unchanged, &unchanged, &quiet_consumption());
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
        let quiet = render_run_summary(&unchanged, &unchanged, &quiet_consumption());
        assert!(
            quiet.contains("intent:    \"test\" (declared at launch)"),
            "{quiet}"
        );
        assert!(quiet.contains("expansions: none declared"), "{quiet}");

        let busy = Consumption {
            intent: Some(("widened".to_string(), "updated mid-session")),
            expansions: 2,
        };
        let out = render_run_summary(&unchanged, &unchanged, &busy);
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
    fn short_oid_is_safe_for_short_input() {
        assert_eq!(short_oid("abc"), "abc");
        assert_eq!(short_oid(""), "");
        assert_eq!(short_oid("0123456789"), "0123456");
    }
}
