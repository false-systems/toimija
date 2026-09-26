//! Agent launch adapters (proposal 0006 Part C): argv composition for known
//! agent CLIs. The composed launch prompt is the one delivery channel with a
//! guarantee — the agent cannot not-see its own argv — so under `toimija run`
//! the packet is in context by construction, not by instruction. Pure string
//! work: no protocol, no IPC, no SDK.

use std::collections::BTreeMap;
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
         reading outside it.\n",
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
         Sessions in the repositories you touch do not count you as a sibling, so \
         declare why you are here and what you will touch \
         (`toimija intent \"<why>\" --scope <path>`); `toimija sessions --outside-repos` \
         lists you.\n",
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
}
