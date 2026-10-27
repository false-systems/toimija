//! Agent launch adapters (proposal 0006 Part C): argv composition for known
//! agent CLIs. The composed launch prompt is the one delivery channel with a
//! guarantee — the agent cannot not-see its own argv — so under `toimija run`
//! the packet is in context by construction. Idle Codex launches carry it in
//! Codex's `developer_instructions` setting instead, which Codex adds to its
//! own instructions; no model request, protocol, IPC, or SDK is needed.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

use crate::config::AgentPromptConfig;

/// The literal that selects positional delivery in `agents:` configuration.
pub const POSITIONAL_PROMPT: &str = "positional";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptStyle {
    /// The prompt travels as the final positional argument (`codex exec`,
    /// `claude -p`).
    Positional,
    /// The prompt follows a literal flag, e.g. `--message <prompt>`.
    Flag(String),
}

impl PromptStyle {
    /// Parse an `agents:<name>.prompt` value. Validated at config load so a
    /// broken entry fails at `toimija doctor`/first command, not mid-launch.
    pub fn from_config(prompt: &str) -> Result<Self, String> {
        if prompt == POSITIONAL_PROMPT {
            return Ok(PromptStyle::Positional);
        }
        if prompt.starts_with('-') && prompt.len() > 1 {
            return Ok(PromptStyle::Flag(prompt.to_string()));
        }
        Err(format!(
            "prompt must be `{POSITIONAL_PROMPT}` or a flag like `--message`, got `{prompt}`"
        ))
    }
}

/// Agent CLIs and their delivery style, matched by basename. The built-ins
/// stay a short explicit table; repository `agents:` configuration extends
/// and overrides it, so any harness can receive the packet inlined without
/// patching this crate. Unknown agents keep the env-var handshake behind an
/// honest banner.
const KNOWN_AGENTS: &[(&str, PromptStyle)] = &[
    ("claude", PromptStyle::Positional),
    ("codex", PromptStyle::Positional),
    ("gemini", PromptStyle::Positional),
];

/// Resolve the delivery style for a program: configured entries win over the
/// built-in table, both matched by basename.
pub fn prompt_style(
    program: &str,
    configured: &BTreeMap<String, AgentPromptConfig>,
) -> Option<PromptStyle> {
    if let Some(agent) = configured.get(basename(program)) {
        // Validated at config load; a direct-parse failure here is a bug, so
        // fall back to positional rather than aborting a launch over it.
        return Some(PromptStyle::from_config(&agent.prompt).unwrap_or(PromptStyle::Positional));
    }
    KNOWN_AGENTS
        .iter()
        .find(|(name, _)| *name == basename(program))
        .map(|(_, style)| style.clone())
}

pub fn is_known_agent(program: &str, configured: &BTreeMap<String, AgentPromptConfig>) -> bool {
    prompt_style(program, configured).is_some()
}

fn basename(program: &str) -> &str {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
}

/// An idle Codex TUI needs context, not a user message that starts a model turn.
/// Other agents retain their configured prompt delivery.
pub fn idle_codex(
    program: &str,
    rest: &[String],
    has_task: bool,
    configured: &BTreeMap<String, AgentPromptConfig>,
) -> bool {
    if basename(program) != "codex" || has_task || configured.contains_key("codex") {
        return false;
    }
    // Only interactive launches wait idle. A subcommand such as `codex exec`
    // or `codex resume` keeps its positional prompt.
    let mut args = rest.iter().peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" | "-h" | "--help" | "-V" | "--version" => return false,
            // `-i/--image` takes one or more files, as Codex's parser does: every
            // value up to the next flag is an image, not a subcommand.
            "-i" | "--image" => {
                if args.next().is_none() {
                    return false;
                }
                while args.next_if(|value| !value.starts_with('-')).is_some() {}
            }
            "-c"
            | "--config"
            | "-m"
            | "--model"
            | "-p"
            | "--profile"
            | "--local-provider"
            | "-s"
            | "--sandbox"
            | "-C"
            | "--cd"
            | "--add-dir"
            | "--enable"
            | "--disable"
            | "-a"
            | "--ask-for-approval"
            | "--remote"
            | "--remote-auth-token-env" => {
                if args.next().is_none() {
                    return false;
                }
            }
            value if value.starts_with('-') => {}
            _ => return false,
        }
    }
    true
}

pub fn launch_args(
    program: &str,
    rest: &[String],
    prompt: String,
    has_task: bool,
    configured: &BTreeMap<String, AgentPromptConfig>,
) -> anyhow::Result<Vec<String>> {
    if idle_codex(program, rest, has_task, configured) {
        // Codex adds `developer_instructions` to its own developer message
        // (skills, permissions, collaboration mode), checked against the
        // offline `codex debug prompt-input` of 0.161.0: the slot carries only
        // toimija's packet and briefing. It does replace a `developer_instructions`
        // set in the user's Codex config for this session.
        return Ok(vec![
            "-c".into(),
            format!("developer_instructions={}", toml_string(&prompt)?),
        ]);
    }
    Ok(
        match prompt_style(program, configured).unwrap_or(PromptStyle::Positional) {
            PromptStyle::Positional => vec![prompt],
            PromptStyle::Flag(flag) => vec![flag, prompt],
        },
    )
}

/// Codex may execute tools in a pre-existing daemon rather than a descendant
/// of the wrapped CLI. Carry this run's handshake in thread configuration too.
/// Dotted keys preserve unrelated shell settings; no inheritance policy changes.
///
/// `set` can only add variables, and that daemon's environment may still hold
/// an earlier session's handshake. So each variable `run` removes from the
/// child (`cleared`) is set empty for the tools; toimija reads an empty
/// handshake value as unset.
pub fn shell_env_args(
    program: &str,
    env: &[(&str, OsString)],
    cleared: &[&str],
) -> anyhow::Result<Vec<String>> {
    let mut args = Vec::new();
    if basename(program) == "codex" {
        for (key, value) in env {
            let value = value.to_str().ok_or_else(|| {
                anyhow::anyhow!("{key} is not UTF-8; cannot deliver the Codex session handshake")
            })?;
            args.push("-c".into());
            args.push(format!(
                "shell_environment_policy.set.{key}={}",
                toml_string(value)?
            ));
        }
        for key in cleared {
            args.push("-c".into());
            args.push(format!("shell_environment_policy.set.{key}=\"\""));
        }
    }
    Ok(args)
}

/// A `-c` value as a TOML basic string. JSON's string escapes are TOML's,
/// except that JSON leaves U+007F raw and TOML forbids it; Codex would then
/// take the whole value as a literal, quotes and escapes included.
fn toml_string(value: &str) -> serde_json::Result<String> {
    Ok(serde_json::to_string(value)?.replace('\u{7f}', "\\u007f"))
}

/// The heading of the launcher's alerts, the first block of a launch prompt.
pub const ALERT_HEADING: &str = "## Before anything else (from the launcher)";

/// Alerts are a few urgent lines, so they stay above the packet without
/// pushing it down; long background belongs in `--briefing`.
pub const ALERT_CAP_BYTES: usize = 1024;

/// The alerts that fit in [`ALERT_CAP_BYTES`] in total, the last one cut at a
/// character boundary, and how many bytes did not fit.
pub fn cap_alerts(alerts: &[String]) -> (Vec<String>, usize) {
    let mut room = ALERT_CAP_BYTES;
    let mut kept = Vec::new();
    let mut cut = 0;
    for alert in alerts {
        if alert.len() <= room {
            room -= alert.len();
            kept.push(alert.clone());
            continue;
        }
        let mut end = room;
        while !alert.is_char_boundary(end) {
            end -= 1;
        }
        if end > 0 {
            kept.push(alert[..end].to_string());
        }
        cut += alert.len() - end;
        room -= end;
    }
    (kept, cut)
}

/// Put the alerts in front of a composed prompt, each with its own text, so
/// the agent reads them before the contract, the packet, the task and the
/// briefing. A prompt with no alerts is left as it is.
pub fn prepend_alerts(prompt: &mut String, alerts: &[String], cut: usize) {
    if alerts.is_empty() && cut == 0 {
        return;
    }
    let mut block = format!("{ALERT_HEADING}\n");
    for alert in alerts {
        block.push_str(alert.trim_end());
        block.push('\n');
    }
    if cut > 0 {
        block.push_str(&format!(
            "(toimija truncated the alerts: {cut} bytes over the {ALERT_CAP_BYTES}-byte cap \
             are not shown)\n"
        ));
    }
    block.push('\n');
    prompt.insert_str(0, &block);
}

/// Compose the launch prompt for a known or configured agent: contract
/// framing, the entry packet inlined (already rendered at the session
/// budget), the live packet path for re-reads, then the human's task. `None`
/// for agents without a resolved delivery style.
pub fn compose_launch_prompt(
    program: &str,
    packet_markdown: &str,
    live_packet: &Path,
    task: Option<&str>,
    configured: &BTreeMap<String, AgentPromptConfig>,
) -> Option<String> {
    prompt_style(program, configured)?;
    let mut prompt = String::new();
    prompt.push_str(
        "Your toimija session packet is below. It is your workset contract: work from it, \
         and declare any expansion (`toimija intent \"<why>\" --scope <path>`) before \
         reading outside it. Use its Git facts, Read first list and validation plan directly; \
         do not repeat discovery unless a fact is missing, stale or contradicted. \
         Read the named files to answer the task's remaining questions.\n",
    );
    prompt.push_str(&format!(
        "Live packet (re-read after HEAD, branch, index, or unfamiliar worktree changes; \
         your own expected source edits do not require a full re-read; run `toimija verify` \
         before commit or handoff): {}\n\n",
        live_packet.display()
    ));
    prompt.push_str(packet_markdown.trim_end());
    prompt.push('\n');
    if let Some(task) = task {
        prompt.push_str(&format!("\nTask: {task}\n"));
    }
    Some(prompt)
}

/// Compose the launch prompt for a session outside any Git repository (issue
/// #225): no packet to inline, and the agent is told so rather than left to
/// look for one. `None` for agents without a resolved delivery style.
pub fn compose_outside_prompt(
    program: &str,
    folder: &Path,
    task: Option<&str>,
    configured: &BTreeMap<String, AgentPromptConfig>,
) -> Option<String> {
    prompt_style(program, configured)?;
    let mut prompt = format!(
        "You are in a toimija session outside any Git repository ({}). There is no \
         session packet: toimija compiles Git context, and this folder has none. \
         When work is assigned, declare why you are here and what you will touch \
         (`toimija intent \"<why>\" --scope <path>`); `toimija sessions --outside-repos` \
         lists you. With no concrete task, acknowledge and wait instead of running \
         discovery or bookkeeping tools. Once a repository is selected, read its \
         Toimija context once and use that workset instead of scanning the workspace.\n",
        folder.display()
    );
    if let Some(task) = task {
        prompt.push_str(&format!("\nTask: {task}\n"));
    }
    Some(prompt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn no_config() -> BTreeMap<String, AgentPromptConfig> {
        BTreeMap::new()
    }

    fn config(entries: &[(&str, &str)]) -> BTreeMap<String, AgentPromptConfig> {
        entries
            .iter()
            .map(|(name, prompt)| {
                (
                    (*name).to_string(),
                    AgentPromptConfig {
                        prompt: (*prompt).to_string(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn known_agents_match_by_basename_only() {
        assert!(is_known_agent("claude", &no_config()));
        assert!(is_known_agent("/usr/local/bin/codex", &no_config()));
        assert!(is_known_agent("gemini", &no_config()));
        assert!(!is_known_agent("sh", &no_config()));
        assert!(!is_known_agent("/usr/bin/claudette", &no_config()));
    }

    #[test]
    fn idle_delivery_preserves_explicit_agent_configuration() {
        let prompt = "context".to_string();
        assert_eq!(
            launch_args(
                "/not-installed/codex",
                &["exec".into()],
                prompt.clone(),
                false,
                &no_config()
            )
            .unwrap(),
            vec!["context"]
        );
        assert!(!idle_codex("codex", &["exec".into()], false, &no_config()));
        assert!(!idle_codex(
            "codex",
            &["resume".into()],
            false,
            &no_config()
        ));
        assert!(!idle_codex(
            "codex",
            &["--help".into()],
            false,
            &no_config()
        ));
        assert!(idle_codex(
            "codex",
            &["-p".into(), "exec".into(), "--no-alt-screen".into()],
            false,
            &no_config()
        ));
        assert_eq!(
            launch_args("codex", &[], prompt.clone(), true, &no_config()).unwrap(),
            vec!["context"]
        );
        assert_eq!(
            launch_args("claude", &[], prompt.clone(), false, &no_config()).unwrap(),
            vec!["context"]
        );
        assert_eq!(
            launch_args(
                "codex",
                &[],
                prompt,
                false,
                &config(&[("codex", "--message")])
            )
            .unwrap(),
            vec!["--message", "context"]
        );
    }

    #[test]
    fn idle_codex_gets_only_the_prompt_and_starts_no_renderer() {
        // A program that does not exist proves nothing is spawned to build
        // the arguments: the slot is filled from the prompt alone.
        assert_eq!(
            launch_args(
                "/not-installed/codex",
                &[],
                "packet\nbriefing".into(),
                false,
                &no_config()
            )
            .unwrap(),
            vec!["-c", "developer_instructions=\"packet\\nbriefing\""]
        );
    }

    #[test]
    fn a_users_own_developer_instructions_keep_the_packet_out_of_that_slot() {
        // Toimija's `-c` would come after the user's and replace it. So a launch
        // that sets the slot itself is not an idle one: its value stays, and
        // the packet travels as the first message, in every form Codex reads.
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        for own in [
            args(&["-c", "developer_instructions=\"mine\""]),
            args(&["--config", "developer_instructions=mine"]),
            args(&["--config=developer_instructions=mine"]),
            args(&["-cdeveloper_instructions=mine"]),
            args(&["-m", "o3", "-c", " developer_instructions = 'mine'"]),
        ] {
            assert!(!idle_codex("codex", &own, false, &no_config()), "{own:?}");
            assert_eq!(
                launch_args("codex", &own, "packet".into(), false, &no_config()).unwrap(),
                vec!["packet"],
                "{own:?}"
            );
        }
        // Other settings, and a key that only starts the same way, stay idle.
        for other in [
            args(&["-c", "model=o3"]),
            args(&["-c", "developer_instructions_file=x"]),
            args(&["--config=model=o3"]),
        ] {
            assert!(
                idle_codex("codex", &other, false, &no_config()),
                "{other:?}"
            );
        }
    }

    #[test]
    fn a_cut_alert_leaves_no_room_for_a_later_one() {
        // The second alert cannot fit one character of `€` (3 bytes) in the
        // byte left, so it is cut whole; the third must not take that byte.
        // The alerts kept are always a prefix of what the launcher sent.
        let alerts = vec![
            "A".repeat(ALERT_CAP_BYTES - 1),
            "€€ second".into(),
            "X third".into(),
        ];
        let (kept, cut) = cap_alerts(&alerts);
        assert_eq!(kept, vec!["A".repeat(ALERT_CAP_BYTES - 1)]);
        assert_eq!(cut, "€€ second".len() + "X third".len());
    }

    #[test]
    fn values_are_toml_strings_even_with_a_delete_character() {
        // TOML forbids a raw U+007F in a basic string, and Codex takes a `-c`
        // value that fails to parse as a literal: the agent would get the
        // quotes and escapes verbatim. JSON leaves U+007F raw.
        let value = "a\u{7f}b \"q\" \\ \n";
        let args = launch_args("codex", &[], value.into(), false, &no_config()).unwrap();
        let encoded = args[1].strip_prefix("developer_instructions=").unwrap();
        assert!(!encoded.contains('\u{7f}'), "{encoded:?}");
        assert!(encoded.contains("\\u007f"), "{encoded:?}");
        assert_eq!(serde_json::from_str::<String>(encoded).unwrap(), value);
        let env = [("TOIMIJA_LIVE", OsString::from(value))];
        let set = shell_env_args("codex", &env, &[]).unwrap();
        assert!(!set[1].contains('\u{7f}'), "{:?}", set[1]);
    }

    #[test]
    fn images_take_every_value_up_to_the_next_flag() {
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        assert!(idle_codex(
            "codex",
            &args(&["-i", "a.png", "b.png", "--no-alt-screen"]),
            false,
            &no_config()
        ));
        assert!(idle_codex(
            "codex",
            &args(&["--image", "a.png", "-m", "o3"]),
            false,
            &no_config()
        ));
        assert!(!idle_codex("codex", &args(&["-i"]), false, &no_config()));
        assert!(!idle_codex(
            "codex",
            &args(&["-m", "o3", "exec"]),
            false,
            &no_config()
        ));
    }

    #[test]
    fn codex_carries_dynamic_handshake_without_changing_environment_policy() {
        let value = "/tmp/session with \"quotes\"/back\\slash\nand-newline";
        let env = [("TOIMIJA_LIVE", OsString::from(value))];
        let args = shell_env_args("/tools/codex", &env, &[]).unwrap();
        assert_eq!(args[0], "-c");
        let encoded = args[1]
            .strip_prefix("shell_environment_policy.set.TOIMIJA_LIVE=")
            .unwrap();
        assert_eq!(serde_json::from_str::<String>(encoded).unwrap(), value);
        assert!(shell_env_args("claude", &env, &["X"]).unwrap().is_empty());
        assert!(
            shell_env_args("codex-wrapper", &env, &["X"])
                .unwrap()
                .is_empty()
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let invalid = [("TOIMIJA_LIVE", OsString::from_vec(vec![0xff]))];
            assert!(shell_env_args("codex", &invalid, &[]).is_err());
        }
    }

    #[test]
    fn configured_agents_extend_and_override_the_builtin_table() {
        let configured = config(&[("aider", "--message"), ("opencode", "positional")]);
        assert_eq!(
            prompt_style("aider", &configured),
            Some(PromptStyle::Flag("--message".into()))
        );
        assert_eq!(
            prompt_style("/opt/opencode/bin/opencode", &configured),
            Some(PromptStyle::Positional)
        );
        // Configuration wins over a built-in, by basename.
        let overridden = config(&[("claude", "--print")]);
        assert_eq!(
            prompt_style("claude", &overridden),
            Some(PromptStyle::Flag("--print".into()))
        );
        // Unconfigured programs are still unknown.
        assert_eq!(prompt_style("aider", &no_config()), None);
    }

    #[test]
    fn prompt_style_config_values_are_validated() {
        assert_eq!(
            PromptStyle::from_config("positional"),
            Ok(PromptStyle::Positional)
        );
        assert_eq!(
            PromptStyle::from_config("-p"),
            Ok(PromptStyle::Flag("-p".into()))
        );
        assert_eq!(
            PromptStyle::from_config("--message"),
            Ok(PromptStyle::Flag("--message".into()))
        );
        for bad in ["", "-", "message", "positional-extra"] {
            assert!(PromptStyle::from_config(bad).is_err(), "{bad} passed");
        }
    }

    #[test]
    fn composed_prompt_carries_contract_packet_path_and_task() {
        let live = PathBuf::from(".toimija/sessions/1-1/current.md");
        let prompt = compose_launch_prompt(
            "codex",
            "# Toimija Context\n\nbody\n",
            &live,
            Some("fix #51"),
            &no_config(),
        )
        .unwrap();
        assert!(prompt.starts_with("Your toimija session packet is below."));
        assert!(prompt.contains("workset contract"));
        // It names the packet's sections as the packet names them.
        assert!(prompt.contains("Use its Git facts, Read first list and validation plan directly"));
        assert!(
            prompt.contains(
                "do not repeat discovery unless a fact is missing, stale or contradicted"
            )
        );
        assert!(prompt.contains("your own expected source edits do not require a full re-read"));
        assert!(prompt.contains("run `toimija verify` before commit or handoff"));
        assert!(prompt.contains(".toimija/sessions/1-1/current.md"));
        assert!(prompt.contains("# Toimija Context"));
        assert!(prompt.ends_with("Task: fix #51\n"));
    }

    #[test]
    fn task_section_is_omitted_when_absent_and_unknown_agents_get_none() {
        let live = PathBuf::from("p.md");
        let prompt = compose_launch_prompt("claude", "packet", &live, None, &no_config()).unwrap();
        assert!(!prompt.contains("Task:"));
        assert!(compose_launch_prompt("bash", "packet", &live, None, &no_config()).is_none());
        // A configured agent composes the same prompt; only the argv shape
        // differs (resolved in workers::prepare, not here).
        let configured = config(&[("aider", "--message")]);
        assert!(compose_launch_prompt("aider", "packet", &live, None, &configured).is_some());
    }

    #[test]
    fn outside_prompt_defers_discovery_until_there_is_work() {
        let prompt =
            compose_outside_prompt("codex", Path::new("/projects"), None, &no_config()).unwrap();
        assert!(prompt.contains("With no concrete task, acknowledge and wait"));
        assert!(prompt.contains("toimija intent"));
        assert!(prompt.contains("Once a repository is selected"));
        assert!(!prompt.contains("Task:"));
    }
}
